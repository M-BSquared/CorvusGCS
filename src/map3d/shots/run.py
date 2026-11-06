#!/usr/bin/env python3
"""Five fixed views of the 3D map, rendered by the real app against a simulated
aircraft and compared with the pictures kept in ref/.

    python3 src/map3d/shots/run.py            # compare, exit 1 on a difference
    python3 src/map3d/shots/run.py --update   # take the pictures again
    python3 src/map3d/shots/run.py --only buildings --keep

Needs: a release build (cargo build --release), Pillow for the comparison
(pip install pillow), the Python environment of python/ (CORVUS_PYTHON, else
.venv/bin/python of the repository or of a sibling checkout), and the network
for the map and elevation tiles (Esri imagery, Copernicus GLO-30). The pictures
are the map area only, with the instrument panel left out, scaled to 1280 by 800
logical pixels, so a retina and a normal screen compare alike. A view differs
when more than 4 percent of its pixels are off by more than 48 of 255.
"""
import argparse
import json
import os
import signal
import socket
import subprocess
import sys
import tempfile
import time
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.abspath(os.path.join(HERE, "..", "..", ".."))
REF = os.path.join(HERE, "ref")
WINDOW = (1280, 800)
MAP_AREA = (70, 60, 900, 800)
MASKS = [(563, 589, 886, 784)]
FULL = {"map": {"three_d_detail": "full"}}
SIMPLE = {"map": {"three_d_detail": "simple"}}

VIEWS = {
    "flat_city": dict(view="11.5755,48.1374,16.5,0,45", config=SIMPLE, city=False),
    "alps_relief": dict(view="11.62,47.42,13,200,60", config=SIMPLE, city=False),
    "far_haze": dict(view="11.4,47.25,10.5,0,60", config=SIMPLE, city=False),
    "ridge_in_front": dict(view="11.57,47.40,14,20,60", config=SIMPLE, city=False),
    "buildings": dict(view="11.5755,48.1374,17,30,60", config=FULL, city=True),
}


def free_port(kind=socket.SOCK_STREAM):
    with socket.socket(socket.AF_INET, kind) as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def python_of_backend():
    if os.environ.get("CORVUS_PYTHON"):
        return os.environ["CORVUS_PYTHON"]
    for base in (ROOT, os.path.join(ROOT, "python"), os.path.dirname(ROOT)):
        cand = os.path.join(base, ".venv", "bin", "python")
        if os.path.exists(cand):
            return cand
    return sys.executable


def post(port, path, body):
    req = urllib.request.Request(
        f"http://127.0.0.1:{port}{path}", data=json.dumps(body).encode(), headers={"Content-Type": "application/json"}
    )
    urllib.request.urlopen(req, timeout=10).read()


def render(name, spec, out, binary, keep):
    py = python_of_backend()
    port, mav = free_port(), free_port(socket.SOCK_DGRAM)
    sandbox = tempfile.mkdtemp(prefix="corvus-3d-shots-")
    tiles = os.path.join(sandbox, ".corvus", "tiles")
    os.makedirs(tiles, exist_ok=True)
    cmd = [py, "tools/scene.py", "run", "flight", "--json", "--port", str(port), "--mav-port", str(mav),
           "--sandbox", sandbox] + spec.get("scene", [])
    if spec["city"]:
        subprocess.run([py, os.path.join(HERE, "make_city.py"), tiles], check=True, stdout=subprocess.DEVNULL)
        cmd.append("--isolated-tiles")
    scene = subprocess.Popen(cmd, cwd=os.path.join(ROOT, "python"), stdout=subprocess.DEVNULL,
                             stderr=subprocess.DEVNULL, start_new_session=True)
    try:
        for _ in range(60):
            try:
                urllib.request.urlopen(f"http://127.0.0.1:{port}/api/version", timeout=1).read()
                break
            except Exception:
                time.sleep(1)
        else:
            raise SystemExit(f"{name}: the simulated aircraft's backend did not start")
        post(port, "/api/config", spec["config"])
        env = dict(os.environ, CORVUS_ALLOW_MULTI="1", CORVUS_GUI_SIZE="%d,%d" % WINDOW,
                   CORVUS_GUI_3D=spec["view"], CORVUS_GUI_SHOT_AFTER="16", CORVUS_GUI_SHOT=out)
        subprocess.run([binary, "--server", f"http://127.0.0.1:{port}"], env=env, check=True,
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=180)
    finally:
        try:
            os.killpg(scene.pid, signal.SIGTERM)
        except OSError:
            pass
        scene.wait(timeout=30)
        if not keep:
            subprocess.run(["rm", "-rf", sandbox])


def prepared(path):
    from PIL import Image, ImageDraw

    img = Image.open(path).convert("RGB").resize(WINDOW, Image.LANCZOS) if Image.open(path).size != WINDOW else Image.open(path).convert("RGB")
    draw = ImageDraw.Draw(img)
    for m in MASKS:
        draw.rectangle(m, fill=(0, 0, 0))
    return img.crop(MAP_AREA)


def differs(a, b):
    from PIL import ImageChops

    diff = ImageChops.difference(a, b).convert("L").point(lambda v: 255 if v > 48 else 0)
    return sum(diff.histogram()[255:]) / (a.width * a.height)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--update", action="store_true", help="keep the pictures taken as the references")
    ap.add_argument("--only", action="append", choices=sorted(VIEWS))
    ap.add_argument("--keep", action="store_true", help="keep the sandboxes and the pictures taken")
    ap.add_argument("--tolerance", type=float, default=0.04)
    ap.add_argument("--binary", default=os.environ.get("CORVUS_BIN", os.path.join(ROOT, "target", "release", "corvus-gcs")))
    args = ap.parse_args()
    failed = []
    with tempfile.TemporaryDirectory(prefix="corvus-3d-out-") as tmp:
        for name in args.only or VIEWS:
            out = os.path.join(tmp, name + ".png")
            render(name, VIEWS[name], out, args.binary, args.keep)
            shot = prepared(out)
            ref_path = os.path.join(REF, name + ".jpg")
            if args.update or not os.path.exists(ref_path):
                shot.save(ref_path, quality=85)
                print(f"{name}: reference written")
                continue
            from PIL import Image

            share = differs(shot, Image.open(ref_path).convert("RGB"))
            ok = share <= args.tolerance
            print(f"{name}: {share * 100:.2f}% of pixels differ, {'ok' if ok else 'FAILED'}")
            if not ok:
                failed.append(name)
                shot.save(os.path.join(HERE, name + ".new.png"))
    if failed:
        print("wrote <name>.new.png beside this script for", ", ".join(failed))
        sys.exit(1)


if __name__ == "__main__":
    main()
