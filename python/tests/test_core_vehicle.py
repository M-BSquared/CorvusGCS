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
import re
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

VOLATILE = ("dir", "revision", "time", "ts", "boot_ms", "seq", "winner")

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


def _same(flying, method, path, body=None, ignore=VOLATILE, pause=0.4):
    (ps, pb) = harness.http(method, flying["python"] + path, body, timeout=40)
    (rs, rb) = harness.http(method, flying["rust"] + path, body, timeout=40)
    out = [f"status: python {ps} != rust {rs}"] if ps != rs else []
    out += harness.diff_json(pb, rb, ignore)
    time.sleep(pause)
    if os.environ.get("CORVUS_HARNESS_TRACE"):
        print(f"{method} {path} {json.dumps(body)[:50]} -> {ps} {json.dumps(pb)[:110]}")
    return [f"{method} {path} {json.dumps(body)[:60]}: {d}" for d in out], (pb, rb)


def _wait_complete(flying, timeout=40.0):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        done = all(harness.http("GET", flying[n] + "/api/params")[1].get("complete") for n in ("python", "rust"))
        if done:
            return
        time.sleep(0.5)
    raise RuntimeError("the parameter download did not finish")


SETUP_PAGES = ["/api/first-setup", "/api/params/metadata", "/api/params/metadata/cache", "/api/motors", "/api/rc", "/api/motors?fresh=1", "/api/rc?fresh=1", "/api/mounting?fresh=1",
               "/api/tuning?fresh=1", "/api/safety?fresh=1"]


def test_the_setup_pages_and_the_parameter_routes_answer_as_python_answers_them(flying, tmp_path) -> None:
    diffs = []

    def step(method, path, body=None, ignore=VOLATILE, pause=0.4):
        out, pair = _same(flying, method, path, body, ignore, pause)
        diffs.extend(out)
        return pair

    for path in SETUP_PAGES:
        step("GET", path)
    step("POST", "/api/params/download", {})
    _wait_complete(flying)
    step("POST", "/api/params/metadata", {})
    time.sleep(6)
    step("GET", "/api/params/metadata", None, ("source",))
    step("POST", "/api/params/metadata", {}, ("source",))
    step("POST", "/api/params/metadata/cache/clear", {})
    step("POST", "/api/first-setup/airframe", {"preset": 4001})
    py, rs = step("GET", "/api/params", None, ("params",))
    assert [p["name"] for p in py["params"]] if "params" in py else True
    for name in ("python", "rust"):
        listed = harness.http("GET", flying[name] + "/api/params")[1]["params"]
        assert listed == sorted(listed, key=lambda p: p["name"])
    py_list = harness.http("GET", flying["python"] + "/api/params")[1]["params"]
    rs_list = harness.http("GET", flying["rust"] + "/api/params")[1]["params"]
    diffs += harness.diff_json(py_list, rs_list)
    # armed: every write is refused the same way
    armed = [("POST", "/api/params/set", {"name": "MPC_XY_VEL_MAX", "value": 8}),
             ("POST", "/api/params/upload", {"params": [{"name": "MPC_XY_VEL_MAX", "value": 8}]}),
             ("POST", "/api/rc/calibrate", {"channels": [{"channel": 1, "min": 1000, "max": 2000, "trim": 1500}]}),
             ("POST", "/api/motors/assign", {"motor": 1, "bank": "MAIN", "pin": 5}),
             ("POST", "/api/autotune", {"axis": "all"}), ("POST", "/api/calibrate", {"type": "gyro"}),
             ("POST", "/api/calibrate/position", {"position": "level"}), ("POST", "/api/calibrate/cancel", {})]
    for method, path, body in armed:
        step(method, path, body)
    step("POST", "/api/mavlink/arm", {"arm": False}, pause=1.0)
    for method, path, body in [
            ("POST", "/api/params/set", {"name": "MPC_XY_VEL_MAX", "value": 8}),
            ("POST", "/api/params/set", {"name": "NO_SUCH_PARAM", "value": 8}),
            ("POST", "/api/params/verify", {"params": [{"name": "MPC_XY_VEL_MAX", "value": 8}], "names": ["MPC_Z_VEL_MAX_UP"]}),
            ("POST", "/api/params/verify", {"params": [{"name": "MPC_XY_VEL_MAX", "value": 9}]}),
            ("POST", "/api/params/verify", {"names": ["NO_SUCH_PARAM"]}),
            ("POST", "/api/params/upload", {"params": [{"name": "MPC_XY_VEL_MAX", "value": 7}, {"name": "NO_SUCH_PARAM", "value": 1}]}),
            ]:
        step(method, path, body, pause=1.0 if path.endswith("upload") else 0.4)
    time.sleep(4)
    step("GET", "/api/params/upload/result")
    step("POST", "/api/motors/assign", {"motor": 1, "bank": "MAIN", "pin": 5}, pause=1.0)
    step("POST", "/api/motors/assign", {"motor": 1, "bank": "MAIN", "pin": 99})
    step("POST", "/api/motors/assign", {"motor": 1, "bank": "MAIN", "pin": 3}, pause=1.0)
    step("POST", "/api/motors/assign", {"motor": 1, "bank": "MAIN", "pin": 3})
    step("POST", "/api/motors/assign", {"motor": 2, "output": None}, pause=1.0)
    step("GET", "/api/motors")
    step("POST", "/api/rc/calibrate", {"channels": [{"channel": 1, "min": 1000, "max": 2000, "trim": 1500, "reversed": False}],
                                       "count": 8}, pause=1.0)
    step("POST", "/api/rc/calibrate", {"channels": [{"channel": 99, "min": 1000, "max": 2000, "trim": 1500}]})
    step("GET", "/api/rc")
    for method, path, body in [
            ("POST", "/api/autotune", {"axis": "all"}), ("POST", "/api/autotune", {"axis": "all", "enabled": False}),
            ("POST", "/api/calibrate", {"type": "gyro"}), ("POST", "/api/calibrate/cancel", {}),
            ("POST", "/api/calibrate/position", {"position": "level"}),
            ("POST", "/api/vibration/stream", {"enabled": True, "rate_hz": 10}),
            ("POST", "/api/vibration/stream", {"enabled": False}),
            ("POST", "/api/tuning/stream", {"enabled": True, "rate_hz": 20}),
            ("POST", "/api/tuning/stream", {"enabled": False}),
            ("POST", "/api/warnings/clear", {}), ("GET", "/api/params/export/target", None)]:
        step(method, path, body, ("dir", "filename") + VOLATILE)
    for fmt in ("qgc", "json", "mission-planner"):
        written = {}
        for name in ("python", "rust"):
            target = tmp_path / name / fmt
            body = {"dir": str(target), "filename": "p", "format": fmt, "params": [
                {"name": "B", "value": 0.1, "type": 9}, {"name": "A", "value": 3, "type": 6},
                {"name": "C", "value": 1e-5}]}
            status, answer = harness.http("POST", flying[name] + "/api/params/export", body)
            assert status == 200, answer
            written[name] = (target / pathlib.Path(answer["path"]).name).read_text()
        if fmt == "json":
            docs = [json.loads(written[n]) for n in ("python", "rust")]
            for doc in docs:
                doc.pop("exported_at"), doc.pop("version")
            assert docs[0] == docs[1], docs
            assert written["python"].replace("1e-05", "0.00001").count("\n") == written["rust"].count("\n")
        else:
            py_text, rs_text = (re.sub(r"Exported [^\n]*|Corvus GCS [^\n]*", "", written[n]) for n in ("python", "rust"))
            assert py_text == rs_text, (py_text, rs_text)
    step("POST", "/api/mavlink/arm", {"arm": True}, pause=1.0)
    assert not diffs, "\n".join(diffs)


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
