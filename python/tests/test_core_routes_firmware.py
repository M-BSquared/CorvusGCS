"""The Rust firmware area held to the Python server.

``serve.py`` and the Rust ``corvus-core`` binary run on identical state folders
and the same request goes to both (``tools/route_harness.py``). The catalogue is
served from caches both read from one folder, so neither reaches the network.
Skipped when the binary is not built:

    cargo build -p corvus_core --bin corvus-core
"""
from __future__ import annotations

import json
import pathlib
import shutil
import sys
import tempfile
import urllib.error
import urllib.request

import pytest

pytest.importorskip("corvus_link")
sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent.parent / "tools"))

import route_harness as harness  # noqa: E402

pytestmark = pytest.mark.skipif(harness.core_binary() is None, reason="corvus-core is not built")

PX4 = [{
    "tag": "v1.17.0", "name": "v1.17.0", "prerelease": False, "published": "2026-01-01T00:00:00Z",
    "boards": [{"name": "px4_fmu-v6x_default.px4", "label": "Pixhawk 6X (FMUv6X)", "size": 10},
               {"name": "cubepilot_cubeorange_default.px4", "label": "Cube Orange", "size": 11}],
}]
ARDUPILOT = [{
    "tag": "ardupilot:Copter/stable", "name": "ArduCopter (stable)", "prerelease": False, "vendor": "ardupilot",
    "vehicle": "Copter", "published": "",
    "boards": [{"name": "arducopter-stable-CubeOrange.apj", "board": "CubeOrange", "label": "CubeOrange", "size": 0,
                "url": "https://firmware.ardupilot.org/Copter/stable/CubeOrange/arducopter.apj"}],
}]


@pytest.fixture(scope="module")
def pair():
    base = pathlib.Path(tempfile.mkdtemp(prefix="corvus-firmware-routes-"))
    folder = base / "firmware"
    folder.mkdir()
    (folder / "catalog.json").write_text(json.dumps({"releases": PX4}), encoding="utf-8")
    (folder / "catalog-ardupilot.json").write_text(json.dumps({"releases": ARDUPILOT}), encoding="utf-8")
    (folder / "px4_fmu-v6x_default.px4").write_bytes(b"cached image")
    seed = harness.standard_seed()
    seed["config"]["firmware_dir"] = str(folder)
    with harness.Pair(seed=seed) as p:
        yield p
    shutil.rmtree(base, ignore_errors=True)


def _raw_post(base: str, path: str, body: bytes) -> tuple[int, object]:
    req = urllib.request.Request(base + path, data=body, method="POST",
                                 headers={"Content-Type": "application/octet-stream"})
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            status, raw = resp.status, resp.read()
    except urllib.error.HTTPError as exc:
        status, raw = exc.code, exc.read()
    return status, json.loads(raw)


def test_the_status_is_the_pythons(pair) -> None:
    diffs = pair.compare_get("/api/firmware/status")
    assert not diffs, "\n".join(diffs)


@pytest.mark.parametrize("query", ["", "?refresh=0", "?refresh=no"])
def test_the_catalogue_answers_as_python_answers(pair, query) -> None:
    diffs = pair.compare_get(f"/api/firmware/catalog{query}")
    assert not diffs, "\n".join(diffs)
    answer = pair.rs_json("GET", f"/api/firmware/catalog{query}")
    assert [r["tag"] for r in answer["releases"]] == ["v1.17.0", "ardupilot:Copter/stable"]
    assert answer["cached"][0]["name"] == "px4_fmu-v6x_default.px4"
    assert answer["suggested"]["vehicle"] == ""


@pytest.mark.parametrize("body", [
    {}, {"release": "v1.17.0"}, {"board": "x"}, {"release": 5, "board": "x"}, {"release": "v1", "board": None},
    {"release": "  ", "board": "x"}, {"release": "v1.17.0", "board": "px4_fmu-v6x_default.px4"},
    {"release": "nope", "board": "nope"},
])
def test_a_flash_request_is_refused_as_python_refuses_it(pair, body) -> None:
    diffs = pair.compare_post("/api/firmware/flash", body)
    assert not diffs, "\n".join(diffs)


def test_a_cancel_with_nothing_running_is_answered_as_python_answers_it(pair) -> None:
    diffs = pair.compare_post("/api/firmware/cancel", {})
    assert not diffs, "\n".join(diffs)


@pytest.mark.parametrize("body", [b"", b"raw image bytes", b'{"image": "x"}'])
def test_an_upload_is_refused_as_python_refuses_it(pair, body) -> None:
    py = _raw_post(pair.py.url, "/api/firmware/upload", body)
    rs = _raw_post(pair.rs.url, "/api/firmware/upload", body)
    assert py[0] == rs[0], (py, rs)
    assert not harness.diff_json(py[1], rs[1]), (py, rs)


def _first_event(url: str) -> str:
    with urllib.request.urlopen(url, timeout=10) as resp:
        head = b""
        while b"\n\n" not in head:
            head += resp.read(1)
        return head.decode().strip()


def test_the_progress_stream_starts_with_the_idle_state(pair) -> None:
    py = _first_event(pair.py.url + "/api/firmware/progress")
    rs = _first_event(pair.rs.url + "/api/firmware/progress")
    assert json.loads(py.split("data:", 1)[1]) == json.loads(rs.split("data:", 1)[1])
    assert py.split("\n")[0] == rs.split("\n")[0] == "event: progress"
