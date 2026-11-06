#!/usr/bin/env python3
"""Soak run of the native interface against a sandboxed simulator.

Starts `scene.py run flight` (a sandboxed Corvus backend with a simulated PX4,
never the operator's own), starts the native interface against it with
`CORVUS_GUI_SOAK=1` (the interface then drives itself, see `src/app/soak.rs`)
and writes one CSV row every `--every` seconds with the resident memory,
thread count and open file descriptors of the interface, the backend and the
scene process. Only processes this script started are measured or stopped.

    python3 python/tools/gui_soak.py --binary target/release/corvus-gcs \\
        --minutes 15 --csv soak.csv

The summary at the end gives the median memory of the first and the last
quarter and the slope after a warm up of two minutes, in MB per minute.
Needs the repository's virtualenv for `scene.py` (`--python`).
"""

from __future__ import annotations

import argparse
import csv
import os
import signal
import statistics
import subprocess
import sys
import time
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


def sample(pid: int) -> tuple[float, int, int] | None:
    """Resident MB, threads and open descriptors of `pid`, or None if gone."""
    try:
        rss = subprocess.run(["ps", "-o", "rss=", "-p", str(pid)], capture_output=True, text=True).stdout.strip()
        if not rss:
            return None
        if sys.platform == "darwin":
            threads = max(0, len(subprocess.run(["ps", "-M", "-p", str(pid)], capture_output=True, text=True).stdout.splitlines()) - 1)
        else:
            threads = int(subprocess.run(["ps", "-o", "nlwp=", "-p", str(pid)], capture_output=True, text=True).stdout.strip() or 0)
        fds = max(0, len(subprocess.run(["lsof", "-p", str(pid)], capture_output=True, text=True).stdout.splitlines()) - 1)
        return int(rss) / 1024.0, threads, fds
    except (ValueError, OSError):
        return None


def listener(port: int) -> int | None:
    out = subprocess.run(["lsof", "-ti", f"tcp:{port}", "-sTCP:LISTEN"], capture_output=True, text=True).stdout.split()
    return int(out[0]) if out else None


def slope(points: list[tuple[float, float]]) -> float:
    """Least squares slope in MB per minute."""
    if len(points) < 3:
        return 0.0
    mx = statistics.fmean(p[0] for p in points)
    my = statistics.fmean(p[1] for p in points)
    den = sum((p[0] - mx) ** 2 for p in points)
    return 0.0 if den == 0 else sum((p[0] - mx) * (p[1] - my) for p in points) / den


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--binary", default=str(ROOT / "target/release/corvus-gcs"))
    ap.add_argument("--python", default=sys.executable, help="interpreter with the backend's packages")
    ap.add_argument("--scene", default="flight")
    ap.add_argument("--port", type=int, default=18770)
    ap.add_argument("--minutes", type=float, default=15)
    ap.add_argument("--every", type=float, default=10)
    ap.add_argument("--step", default="4", help="seconds per driver step (CORVUS_GUI_SOAK_STEP)")
    ap.add_argument("--csv", default="soak.csv")
    a = ap.parse_args()
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(1))

    scene = subprocess.Popen(
        [a.python, str(ROOT / "python/tools/scene.py"), "run", a.scene, "--port", str(a.port), "--seconds", "0"],
        cwd=ROOT / "python", stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True,
    )
    app = None
    rows: list[dict] = []
    try:
        for _ in range(120):
            try:
                urllib.request.urlopen(f"http://127.0.0.1:{a.port}/api/version", timeout=2).read()
                break
            except OSError:
                time.sleep(1)
        else:
            print("the sandboxed backend did not come up", file=sys.stderr)
            return 1
        env = dict(os.environ, CORVUS_ALLOW_MULTI="1", CORVUS_GUI_SOAK="1", CORVUS_GUI_SOAK_STEP=a.step)
        app = subprocess.Popen([a.binary, "--server", f"http://127.0.0.1:{a.port}"], env=env, cwd=ROOT,
                               stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        t0 = time.time()
        with open(a.csv, "w", newline="") as fh:
            w = csv.writer(fh)
            w.writerow(["t_s", "app_mb", "app_threads", "app_fds", "backend_mb", "backend_threads", "backend_fds"])
            while time.time() - t0 < a.minutes * 60:
                if scene.poll() is not None:
                    print(f"the sandboxed backend stopped after {time.time() - t0:.0f} s (not by this script)", file=sys.stderr)
                    break
                if app.poll() is not None:
                    print(f"the interface exited with {app.returncode} after {time.time() - t0:.0f} s", file=sys.stderr)
                    break
                be = listener(a.port)
                s_app, s_be = sample(app.pid), (sample(be) if be else None)
                if s_app:
                    row = [round(time.time() - t0), *map(lambda v: round(v, 1), s_app), *(map(lambda v: round(v, 1), s_be) if s_be else ("", "", ""))]
                    w.writerow(row)
                    fh.flush()
                    rows.append({"t": row[0], "app": s_app[0], "be": s_be[0] if s_be else None, "threads": s_app[1], "fds": s_app[2]})
                    print(*row, flush=True)
                time.sleep(a.every)
    finally:
        if app and app.poll() is None:
            app.send_signal(signal.SIGTERM)
            try:
                app.wait(15)
            except subprocess.TimeoutExpired:
                app.kill()
        try:
            os.killpg(scene.pid, signal.SIGINT)
            scene.wait(20)
        except (ProcessLookupError, subprocess.TimeoutExpired):
            os.killpg(scene.pid, signal.SIGKILL)

    if len(rows) >= 8:
        q = len(rows) // 4
        for name in ("app", "be"):
            col = [r[name] for r in rows if r[name] is not None]
            if len(col) >= 8:
                tail = [(r["t"] / 60, r[name]) for r in rows if r[name] is not None and r["t"] >= 120]
                print(f"{name}: first quarter {statistics.median(col[:q]):.0f} MB, last quarter {statistics.median(col[-q:]):.0f} MB, "
                      f"max {max(col):.0f} MB, slope after 2 min {slope(tail):+.2f} MB/min")
        print(f"app threads {rows[0]['threads']} -> {rows[-1]['threads']} (max {max(r['threads'] for r in rows)}), "
              f"fds {rows[0]['fds']} -> {rows[-1]['fds']} (max {max(r['fds'] for r in rows)})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
