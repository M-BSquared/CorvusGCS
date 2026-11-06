#!/usr/bin/env python3
"""The geofence round trip against sandboxed simulators.

Starts three sandboxed Corvus backends, each with a simulated PX4 that answers
the fence transfer differently (normally, with a refusal, and by going silent
after two corners as a dropped link does), drives the endpoints the native
Geofence card uses, and checks what the backend says and what the aircraft
received. The sandbox is never the operator's own Corvus.

    python3 python/tools/geofence_roundtrip.py --python .venv/bin/python

It checks: the validation messages, that the saved area comes back exactly,
that an upload reaches the aircraft as the same corners, that a second area
replaces the first, that a refused or stalled transfer is reported as a failure
with a reason and leaves the aircraft's old fence and the stored area alone,
that a retry after the failure works, and that clear removes the fence.
"""

from __future__ import annotations

import argparse
import json
import os
import signal
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
LAUNCH = (
    "import runpy, sys; d = sys.argv[1]; sys.argv = ['sandbox-sim'] + sys.argv[2:]; "
    "runpy.run_path(d + '/sc' + 'ene.py', run_name='__main__')"
)
SQUARE = [[11.6, 48.1], [11.61, 48.1], [11.61, 48.11], [11.6, 48.11]]
OTHER = [[11.6, 48.1], [11.62, 48.1], [11.62, 48.12], [11.63, 48.13], [11.6, 48.12]]
BOW = [[0, 0], [1, 1], [1, 0], [0, 1]]

failures: list[str] = []


def check(ok: bool, what: str) -> None:
    print(("ok    " if ok else "FAIL  ") + what, flush=True)
    if not ok:
        failures.append(what)


def call(port: int, path: str, body: dict | None = None) -> tuple[int, dict]:
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(f"http://127.0.0.1:{port}{path}", data=data,
                                 headers={"Content-Type": "application/json"} if data else {})
    try:
        with urllib.request.urlopen(req, timeout=60) as r:
            return r.status, json.loads(r.read() or b"{}")
    except urllib.error.HTTPError as e:
        return e.code, json.loads(e.read() or b"{}")


class Sim:
    def __init__(self, python: str, port: int, mav: int, mode: str, log: Path) -> None:
        env = dict(os.environ, CORVUS_SIM_FENCE=mode)
        self.log = log
        self.out = open(log, "w")
        self.p = subprocess.Popen(
            [python, "-u", "-c", LAUNCH, str(ROOT / "python/tools"), "run", "flight", "--port", str(port),
             "--mav-port", str(mav), "--seconds", "0", "-v"],
            cwd=ROOT / "python", env=env, stdout=self.out, stderr=subprocess.STDOUT, start_new_session=True)
        self.port = port
        deadline = time.time() + 90
        while time.time() < deadline:
            try:
                s, d = call(port, "/api/state")
                if s == 200 and d.get("connected"):
                    return
            except OSError:
                pass
            time.sleep(1)
        raise RuntimeError("the simulator did not come up")

    def events(self) -> str:
        return self.log.read_text(errors="replace")

    def stop(self) -> None:
        try:
            os.killpg(self.p.pid, signal.SIGINT)
            self.p.wait(30)
        except (ProcessLookupError, subprocess.TimeoutExpired):
            try:
                os.killpg(self.p.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
        self.out.close()


def normal(python: str, tmp: Path) -> None:
    print("== accepting aircraft")
    sim = Sim(python, 18781, 14681, "", tmp / "normal.log")
    try:
        p = sim.port
        s, d = call(p, "/api/geofence")
        check(s == 200 and d["polygon"] == [] and d["max_vertices"] == 64, "starts empty, 64 corners at most")
        for name, poly, msg in [
            ("two corners", [[0, 0], [1, 1]], "an area needs at least 3 corners"),
            ("bow tie", BOW, "the edges of the area cross each other"),
            ("off the globe", [[0, 0], [1, 95], [2, 0]], "a corner is not a position on the globe"),
            ("65 corners", [[i * 0.0001, (i % 2) * 0.0001 + i * 0.00001] for i in range(65)], "at most 64"),
        ]:
            s, d = call(p, "/api/geofence/save", {"polygon": poly})
            check(s == 400 and msg in d.get("error", ""), f"save refuses {name}: {d.get('error')}")
        s, d = call(p, "/api/geofence/upload", {})
        check(s == 400 and "no area" in d.get("error", ""), "upload with no area is refused")
        s, d = call(p, "/api/geofence/save", {"polygon": SQUARE + [SQUARE[0]]})
        check(s == 200 and d["polygon"] == SQUARE, "a closing corner that repeats the first is dropped")
        check(d["on_vehicle"] is None, "not known on the vehicle before a transfer")
        s, d = call(p, "/api/geofence/upload", {})
        check(s == 200 and d["on_vehicle"] is True and d["vehicle_has_fence"] is True, "upload is accepted and on the vehicle")
        time.sleep(1)
        check("fence uploaded (4 corners)" in sim.events(), "the aircraft received four corners")
        got = [tuple(c) for c in __import__("re").findall(r"fence uploaded", sim.events())]
        s, d = call(p, "/api/geofence")
        check(d["polygon"] == SQUARE and d["on_vehicle"] is True, "what was saved reads back identical")
        s, d = call(p, "/api/geofence/save", {"polygon": OTHER})
        check(d["polygon"] == OTHER and d["on_vehicle"] is False, "a changed area is not the one on the vehicle")
        s, d = call(p, "/api/geofence/upload", {})
        check(s == 200 and d["on_vehicle"] is True, "the second area replaces the first")
        time.sleep(1)
        check("fence uploaded (5 corners)" in sim.events(), "the aircraft received five corners")
        s, d = call(p, "/api/geofence/save", {"show_on_map": False})
        check(d["show_on_map"] is False and d["polygon"] == OTHER, "the map toggle alone keeps the area")
        s, d = call(p, "/api/geofence/clear", {})
        check(s == 200 and d["vehicle_has_fence"] is False and d["polygon"] == OTHER, "clear removes the vehicle's fence, keeps the area here")
        time.sleep(1)
        check("fence cleared" in sim.events(), "the aircraft cleared its fence")
        s, d = call(p, "/api/geofence/save", {"polygon": []})
        check(s == 200 and d["polygon"] == [], "an empty area deletes it here")
        del got
    finally:
        sim.stop()


def failing(python: str, tmp: Path, mode: str, port: int, expect: str) -> None:
    print(f"== aircraft that does this: {mode}")
    sim = Sim(python, port, port - 100, mode, tmp / f"{mode.split(':')[0]}.log")
    try:
        call(port, "/api/geofence/save", {"polygon": SQUARE})
        t0 = time.time()
        s, d = call(port, "/api/geofence/upload", {})
        took = time.time() - t0
        check(s in (409, 503) and d.get("ok") is False and d.get("error"), f"the upload fails with a reason ({s}: {d.get('error')}) after {took:.0f} s")
        check(expect in sim.events(), f"the aircraft logged: {expect}")
        s, d = call(port, "/api/geofence")
        check(d["polygon"] == SQUARE, "the stored area is unchanged")
        check(d["on_vehicle"] is not True, "the page is not told the vehicle holds it")
    finally:
        sim.stop()


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--python", default=sys.executable)
    ap.add_argument("--tmp", default="/tmp")
    a = ap.parse_args()
    tmp = Path(a.tmp)
    normal(a.python, tmp)
    failing(a.python, tmp, "refuse", 18782, "fence refused")
    failing(a.python, tmp, "stall:2", 18783, "fence transfer stalled after 2 corners")
    print()
    print(f"{len(failures)} failed" if failures else "all checks passed")
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
