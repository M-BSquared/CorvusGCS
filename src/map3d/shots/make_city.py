"""Seeds a tile cache directory with a dense city centre (515 buildings around
Marienplatz, Munich) through the real BuildingService and a local stand-in for
Overpass, so the 3D view can be looked at where the sandbox blocks Overpass.

usage: make_city.py CACHE_DIR"""
import json, math, os, random, sys, tempfile, threading, time
from http.server import BaseHTTPRequestHandler, HTTPServer
sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", "..", "python"))
from corvus.buildings import BuildingService, CELL_ZOOM

LON0, LAT0 = 11.5755, 48.1374
M_LAT = 1 / 111_320.0
M_LON = 1 / (111_320.0 * math.cos(math.radians(LAT0)))
rng = random.Random(7)

def pt(e, n): return {"lon": LON0 + e * M_LON, "lat": LAT0 + n * M_LAT}
def rect(e0, n0, w, d): return [pt(e0, n0), pt(e0 + w, n0), pt(e0 + w, n0 + d), pt(e0, n0 + d), pt(e0, n0)]

elements, ident = [], 1
for bi in range(-6, 7):
    for bj in range(-6, 7):
        e0, n0 = bi * 70.0, bj * 70.0
        for k in range(rng.randint(2, 4)):
            w, d = rng.uniform(14, 30), rng.uniform(14, 30)
            e, n = e0 + rng.uniform(0, 36), n0 + rng.uniform(0, 36)
            tags = {"building": rng.choice(["yes", "apartments", "commercial", "house"])}
            if rng.random() < 0.6:
                tags["height"] = str(round(rng.uniform(9, 55), 1))
            else:
                tags["building:levels"] = str(rng.randint(2, 12))
            elements.append({"type": "way", "id": ident, "tags": tags, "geometry": rect(e, n, w, d)})
            ident += 1
answer = {"elements": elements}

class H(BaseHTTPRequestHandler):
    def do_POST(self):
        self.rfile.read(int(self.headers.get("Content-Length", 0)))
        body = json.dumps(answer).encode()
        self.send_response(200); self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body))); self.end_headers(); self.wfile.write(body)
    def log_message(self, *a): pass

srv = HTTPServer(("127.0.0.1", 0), H)
threading.Thread(target=srv.serve_forever, daemon=True).start()
os.makedirs(sys.argv[1], exist_ok=True)
svc = BuildingService(os.path.join(sys.argv[1], "buildings.sqlite"), url=f"http://127.0.0.1:{srv.server_port}/api/interpreter")
def cell(lon, lat):
    n = 1 << CELL_ZOOM
    return int((lon + 180) / 360 * n), int((1 - math.asinh(math.tan(math.radians(lat))) / math.pi) / 2 * n)
cells = {cell(LON0 + e * M_LON, LAT0 + n * M_LAT) for e in (-500, 0, 500) for n in (-500, 0, 500)}
got = 0
for x, y in sorted(cells):
    for _ in range(150):
        payload, coming = svc.lookup(CELL_ZOOM, x, y)
        if payload is not None:
            got += 1
            break
        time.sleep(0.1)
svc.close(); srv.shutdown()
print("buildings", len(elements), "cells", sorted(cells), "stored", got)
