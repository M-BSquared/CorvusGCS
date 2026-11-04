"""The real backend process, started, used and stopped.

The shutdown tests elsewhere call the stop sequence inside the test process.
These start ``serve.py`` itself with a simulated vehicle on UDP, send it
SIGINT or SIGTERM, and check what an operator who reboots the laptop between
flights depends on: it exits with code 0 within a few seconds, writes no
traceback, and leaves its ports free. One run keeps the RTK session thread
running against a live NTRIP caster; one lets a process that never called
``stop()`` simply end, with the link's Rust threads still running.
"""
from __future__ import annotations

import json
import os
import signal
import socket
import subprocess
import sys
import threading
import time
import urllib.request
from pathlib import Path
from typing import Any

import pytest

pytest.importorskip("corvus_link")
if os.name == "nt":
    pytest.skip("signals and the process model are POSIX here", allow_module_level=True)

ROOT = Path(__file__).resolve().parent.parent

SIM = (
    "import sys, time, signal; sys.path.insert(0, '.'); sys.path.insert(0, 'tools');"
    "from scene_kit.vehicle import SimVehicle;"
    "v = SimVehicle('udpout:127.0.0.1:' + sys.argv[1]); v.start();"
    "signal.signal(signal.SIGTERM, lambda *a: (v.stop(), sys.exit(0)));"
    "[time.sleep(0.2) for _ in iter(int, 1)]"
)


def free_port(kind: int = socket.SOCK_STREAM) -> int:
    with socket.socket(socket.AF_INET, kind) as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def get(port: int, path: str) -> Any:
    with urllib.request.urlopen(f"http://127.0.0.1:{port}{path}", timeout=3) as response:
        return json.loads(response.read())


def post(port: int, path: str, body: dict) -> Any:
    request = urllib.request.Request(
        f"http://127.0.0.1:{port}{path}", data=json.dumps(body).encode(), headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(request, timeout=3) as response:
        return json.loads(response.read())


def wait_for(predicate: Any, timeout: float = 15.0) -> bool:
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        try:
            if predicate():
                return True
        except Exception:  # noqa: BLE001 - the server may not be up yet
            pass
        time.sleep(0.1)
    return False


class Backend:
    """``serve.py`` in a process of its own, with a home folder of its own."""

    def __init__(self, tmp_path: Path, connection: str) -> None:
        self.http = free_port()
        self.log_path = tmp_path / "serve.log"
        home = tmp_path / "home"
        home.mkdir()
        env = {**os.environ, "HOME": str(home), "USERPROFILE": str(home)}
        env.pop("VIRTUAL_ENV", None)
        self.log = open(self.log_path, "wb")  # noqa: SIM115 - closed in stop()
        self.process = subprocess.Popen(
            [sys.executable, "serve.py", str(self.http), connection],
            cwd=ROOT, env=env, stdout=self.log, stderr=subprocess.STDOUT)

    def up(self) -> bool:
        return wait_for(lambda: get(self.http, "/api/version")["product"] == "Corvus GCS", 30)

    def text(self) -> str:
        if not self.log.closed:
            self.log.flush()
        return self.log_path.read_text(errors="replace")

    def stop(self, sig: int, timeout: float = 15.0) -> tuple[int | None, float]:
        started = time.monotonic()
        self.process.send_signal(sig)
        try:
            code: int | None = self.process.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait()
            code = None
        self.log.close()
        return code, time.monotonic() - started


class Sim:
    def __init__(self, udp_port: int) -> None:
        self.process = subprocess.Popen([sys.executable, "-c", SIM, str(udp_port)], cwd=ROOT,
                                        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

    def stop(self) -> None:
        self.process.send_signal(signal.SIGTERM)
        try:
            self.process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.process.kill()


@pytest.fixture
def running(tmp_path: Path) -> Any:
    made: list[Any] = []

    def start(udp: int) -> tuple[Backend, Sim]:
        backend = Backend(tmp_path, f"udpin:127.0.0.1:{udp}")
        sim = Sim(udp)
        made.extend([backend, sim])
        return backend, sim

    yield start
    for thing in made:
        if isinstance(thing, Backend) and thing.process.poll() is None:
            thing.process.kill()
        if isinstance(thing, Sim):
            thing.stop()


def ports_are_free(http: int, udp: int | None = None) -> bool:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        try:
            s.bind(("127.0.0.1", http))
        except OSError:
            return False
    if udp is not None:
        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as u:
            try:
                u.bind(("127.0.0.1", udp))
            except OSError:
                return False
    return True


@pytest.mark.parametrize("sig", [signal.SIGINT, signal.SIGTERM], ids=["SIGINT", "SIGTERM"])
def test_a_backend_with_a_vehicle_stops_cleanly_on_a_signal(sig: int, running: Any) -> None:
    udp = free_port(socket.SOCK_DGRAM)
    backend, _sim = running(udp)
    assert backend.up(), backend.text()
    assert wait_for(lambda: get(backend.http, "/api/state")["connected"] is True), backend.text()
    state = get(backend.http, "/api/state")
    assert state["autopilot_stack"] == "px4" and state["vehicle_type"] == "QUADROTOR"
    code, took = backend.stop(sig)
    assert code == 0, backend.text()[-1500:]
    assert took < 10
    text = backend.text()
    assert "Traceback" not in text and "Exception in thread" not in text and "panicked" not in text
    assert ports_are_free(backend.http, udp)


def test_a_backend_stops_while_the_rtk_thread_holds_a_caster_stream(running: Any, tmp_path: Path) -> None:
    from tests.test_rtk_service import _rtcm

    seen: dict = {"requests": [], "gone": threading.Event()}
    listen = socket.socket()
    listen.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    listen.bind(("127.0.0.1", 0))
    listen.listen(2)
    caster_port = listen.getsockname()[1]

    def caster() -> None:
        listen.settimeout(0.2)
        while not seen["gone"].is_set():
            try:
                conn, _ = listen.accept()
            except (TimeoutError, OSError):
                continue
            seen["requests"].append(conn.recv(4096))
            conn.sendall(b"ICY 200 OK\r\n\r\n")
            try:
                while True:
                    conn.sendall(_rtcm(1005) + _rtcm(1074, bytes(200)))
                    time.sleep(0.2)
            except OSError:
                seen["gone"].set()

    threading.Thread(target=caster, daemon=True).start()
    udp = free_port(socket.SOCK_DGRAM)
    backend, _sim = running(udp)
    try:
        assert backend.up(), backend.text()
        assert wait_for(lambda: get(backend.http, "/api/state")["connected"] is True)
        post(backend.http, "/api/rtk/settings", {
            "enabled": True, "source": "ntrip",
            "ntrip": {"host": "127.0.0.1", "port": caster_port, "mountpoint": "MSM4", "username": "u", "password": "secret"}})
        assert wait_for(lambda: get(backend.http, "/api/rtk/status")["state"] == "active"), backend.text()[-1500:]
        assert wait_for(lambda: get(backend.http, "/api/rtk/status")["injected"]["messages"] >= 4)
        status = get(backend.http, "/api/rtk/status")
        assert status["settings"]["ntrip"]["password"] == "" and status["settings"]["ntrip"]["has_password"] is True
        assert "secret" not in json.dumps(status)
        assert b"GET /MSM4" in seen["requests"][0] and b"Authorization: Basic" in seen["requests"][0]
        code, took = backend.stop(signal.SIGINT)
        assert code == 0, backend.text()[-1500:]
        assert took < 10
        assert seen["gone"].wait(5), "the caster still holds a client"
        assert ports_are_free(backend.http, udp)
        assert "Traceback" not in backend.text()
    finally:
        seen["gone"].set()
        listen.close()


def test_a_process_that_never_stops_its_link_still_exits(tmp_path: Path) -> None:
    code = (
        "import sys, time, logging; logging.disable(logging.CRITICAL);"
        "sys.path.insert(0, '.'); sys.path.insert(0, 'tools');"
        "from scene_kit.vehicle import SimVehicle;"
        "from corvus.mavlink_bridge import MavlinkBridge;"
        "from corvus.state_store import VehicleStateStore;"
        "port = sys.argv[1]; bridge = MavlinkBridge(VehicleStateStore(), 'udpin:127.0.0.1:' + port); bridge.start();"
        "v = SimVehicle('udpout:127.0.0.1:' + port); v.start();"
        "[time.sleep(0.1) for _ in range(80) if not bridge.is_connected()];"
        "print('connected', bridge.is_connected()); v.stop()"
    )
    started = time.monotonic()
    done = subprocess.run([sys.executable, "-c", code, str(free_port(socket.SOCK_DGRAM))], cwd=ROOT,
                          capture_output=True, text=True, timeout=30)
    assert done.returncode == 0, done.stderr[-1500:]
    assert "connected True" in done.stdout
    assert time.monotonic() - started < 25
    assert "Traceback" not in done.stderr and "Fatal Python error" not in done.stderr
