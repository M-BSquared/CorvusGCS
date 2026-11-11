"""The Rust core flying a simulated vehicle, held to the Python server doing the same.

Two copies of the simulated PX4 of ``tools/scene_kit`` (one per backend, so the
flights do not disturb each other), ``serve.py`` on one and ``corvus-core
--mavlink`` on the other, then the same requests to both. Skipped when
``corvus-core`` is not built.
"""
from __future__ import annotations

import json
import os
import pathlib
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time

import pytest

pytest.importorskip("corvus_link")
sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent.parent / "tools"))

import route_harness as harness  # noqa: E402

pytestmark = pytest.mark.skipif(harness.core_binary() is None, reason="corvus-core is not built")

PYTHON_ROOT = harness.PYTHON_ROOT


def _wait_state(url: str, timeout: float = 40.0) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            status, body = harness.http("GET", url + "/api/state", timeout=3)
            if status == 200 and isinstance(body, dict) and body.get("connected") and body.get("mode") not in ("", "DISCONNECTED"):
                return
        except OSError:
            pass
        time.sleep(0.3)
    raise RuntimeError(f"{url} never saw the aircraft")


@pytest.fixture(scope="module")
def flying():
    tmp = pathlib.Path(tempfile.mkdtemp(prefix="corvus-vehicle-"))
    procs: list[subprocess.Popen] = []
    logs = []

    def spawn(args, env=None, cwd=None, name="proc"):
        log = open(tmp / f"{name}.log", "wb")
        logs.append(log)
        p = subprocess.Popen(args, env=env, cwd=cwd, stdout=log, stderr=subprocess.STDOUT)
        procs.append(p)
        return p

    try:
        servers = {}
        for name in ("python", "rust"):
            mav = harness.free_port(socket.SOCK_DGRAM)
            http_port = harness.free_port()
            state = tmp / name / ".corvus"
            harness.seed_state(state, harness.standard_seed())
            sim_env = dict(os.environ, PYTHONUNBUFFERED="1")
            spawn([sys.executable, "tools/scene.py", "run", "flight", "--no-backend", "--mav-port", str(mav),
                   "--seconds", "900", "--sandbox", str(tmp / f"{name}-scene")], env=sim_env, cwd=PYTHON_ROOT, name=f"{name}-sim")
            if name == "python":
                env = dict(os.environ, HOME=str(tmp / name), USERPROFILE=str(tmp / name), CORVUS_ALLOW_MULTI="1")
                spawn([sys.executable, "serve.py", str(http_port), f"udpin:127.0.0.1:{mav}"], env=env, cwd=PYTHON_ROOT, name=name)
            else:
                env = dict(os.environ, CORVUS_ALLOW_MULTI="1")
                spawn([str(harness.core_binary()), "--port", str(http_port), "--state-dir", str(state),
                       "--mavlink", f"udpin:127.0.0.1:{mav}"], env=env, name=name)
            servers[name] = f"http://127.0.0.1:{http_port}"
        for url in servers.values():
            _wait_state(url)
        yield servers
    finally:
        for p in procs:
            if p.poll() is None:
                p.send_signal(signal.SIGTERM)
        for p in procs:
            try:
                p.wait(timeout=15)
            except subprocess.TimeoutExpired:
                p.kill()
        for log in logs:
            log.close()
        if not os.environ.get("CORVUS_HARNESS_KEEP"):
            shutil.rmtree(tmp, ignore_errors=True)


PLAN = {"name": "Circuit", "aircraft": "multirotor", "speed": 5, "items": [
    {"type": "takeoff", "lat": 48.0757, "lon": 11.642, "alt": 20},
    {"type": "loiter_turns", "lat": 48.0760, "lon": 11.6430, "alt": 25, "turns": 1, "radius": 20},
    {"type": "land", "lat": 48.0757, "lon": 11.642}]}

VOLATILE = ("revision", "time", "ts", "boot_ms", "seq", "winner")

# One flight, in order: what the page would send.
SEQUENCE = [
    ("GET", "/api/mavlink/capabilities", None), ("GET", "/api/mavlink/modes", None),
    ("GET", "/api/mavlink/auto", None), ("POST", "/api/mavlink/arm", {"arm": False}),
    ("POST", "/api/mavlink/mode", {"mode": "hold"}), ("POST", "/api/mavlink/mode", {"mode": "loiter"}),
    ("POST", "/api/mavlink/hold", {}), ("POST", "/api/mavlink/arm", {"arm": True}),
    ("POST", "/api/mavlink/takeoff", {"altitude": 20}), ("POST", "/api/mavlink/sethome", {"lat": 48.07, "lon": 11.64}),
    ("POST", "/api/mission/upload", {"plan": PLAN}), ("POST", "/api/mission/download", {}),
    ("POST", "/api/mission/start", {"items": 3}), ("POST", "/api/mission/clear", {}),
    ("POST", "/api/mavlink/gotopoints", {"points": [{"lat": 48.0757, "lon": 11.642, "alt_agl": 30}]}),
    ("POST", "/api/mavlink/command", {"command": "DO_CHANGE_SPEED", "params": [1, 5, -1]}),
    ("POST", "/api/mavlink/velocity", {"vx": 1}), ("POST", "/api/mavlink/manual", {"x": 0.1}),
    ("POST", "/api/mavlink/shell/send", {"data": "ver\n"}), ("POST", "/api/mavlink/shell/close", {}),
    ("POST", "/api/mavlink/rtl", {}), ("POST", "/api/mavlink/land", {}), ("POST", "/api/mavlink/reboot", {}),
    ("POST", "/api/geofence/save", {"polygon": [[11.64, 48.07], [11.65, 48.07], [11.65, 48.08]]}),
    ("POST", "/api/geofence/upload", {}), ("GET", "/api/geofence", None), ("POST", "/api/geofence/clear", {}),
    ("POST", "/api/motors/test", {"motor": 1}), ("POST", "/api/motors/test/stop", {}),
    ("POST", "/api/rangefinder/stream", {"enabled": True}), ("POST", "/api/rc/stream", {"enabled": True, "rate_hz": 10}),
    ("POST", "/api/mavlink/disconnect", {}), ("GET", "/api/mavlink/auto", None),
]


def test_a_flight_is_answered_as_python_answers_it(flying) -> None:
    diffs = []
    for method, path, body in SEQUENCE:
        (ps, pb) = harness.http(method, flying["python"] + path, body)
        (rs, rb) = harness.http(method, flying["rust"] + path, body)
        out = [f"status: python {ps} != rust {rs}"] if ps != rs else []
        out += harness.diff_json(pb, rb, VOLATILE)
        diffs += [f"{method} {path} {json.dumps(body)[:60]}: {d}" for d in out]
        time.sleep(0.4)
    assert not diffs, "\n".join(diffs)
