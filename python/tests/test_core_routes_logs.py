"""The Rust logs area held to the Python server.

``serve.py`` and the Rust ``corvus-core`` binary run on identical state folders
and the same request goes to both (``tools/route_harness.py``), with the flight
and telemetry kits (``tools/logs_kit.py``, ``tools/tlog_kit.py``) as the files
to review. Both servers read the same two folders, so every path in an answer is
the same text. Skipped when the binary is not built:

    cargo build -p corvus_core --bin corvus-core
"""
from __future__ import annotations

import json
import os
import pathlib
import shutil
import sys
import tempfile
import urllib.error
import urllib.request

import pytest

pytest.importorskip("corvus_link")
sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent.parent / "tools"))

import logs_kit  # noqa: E402
import route_harness as harness  # noqa: E402
import tlog_kit  # noqa: E402

pytestmark = pytest.mark.skipif(harness.core_binary() is None, reason="corvus-core is not built")

FLIGHTS = ["clean_quad", "legacy_names", "saturated_motor", "clipping"]
TLOGS = ["clean", "ardupilot_modes", "damaged_tail", "bad_header", "header_only"]


@pytest.fixture(scope="module")
def folders():
    base = pathlib.Path(tempfile.mkdtemp(prefix="corvus-logs-routes-"))
    downloads, recordings = base / "flightlogs", base / "recordings"
    downloads.mkdir()
    recordings.mkdir()
    flights = logs_kit.build_flights()
    for i, name in enumerate(FLIGHTS):
        (downloads / f"log_{i + 1:03d}_{name}.ulg").write_bytes(flights[name]())
    (downloads / "log_009.bin").write_bytes(b"\x00" * 64)
    (downloads / "notes.txt").write_text("not a log", encoding="utf-8")
    tlogs = tlog_kit.build_tlogs()
    for i, name in enumerate(TLOGS):
        (recordings / f"20260910-1{i}0000-000000.tlog").write_bytes(tlogs[name]())
    (recordings / "from-a-colleague.tlog").write_bytes(tlogs["clean"]())
    yield downloads, recordings
    shutil.rmtree(base, ignore_errors=True)


@pytest.fixture(scope="module")
def pair(folders):
    downloads, recordings = folders
    seed = harness.standard_seed()
    seed["config"].update({
        "log_download_dir": str(downloads),
        "tlog_dir": str(recordings),
        "review": {"sensitivity": "strict"},
    })
    with harness.Pair(seed=seed) as p:
        yield p


def _raw_post(base: str, path: str, body: bytes) -> tuple[int, object]:
    req = urllib.request.Request(base + path, data=body, method="POST",
                                 headers={"Content-Type": "application/octet-stream"})
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            status, raw = resp.status, resp.read()
    except urllib.error.HTTPError as exc:
        status, raw = exc.code, exc.read()
    return status, json.loads(raw)


def test_the_status_lists_the_folders_as_python_does(pair) -> None:
    diffs = pair.compare_get("/api/logs/status")
    assert not diffs, "\n".join(diffs)
    status = pair.rs_json("GET", "/api/logs/status")
    assert len(status["saved"]) == len(FLIGHTS) + 1
    assert len(status["tlogs"]) == len(TLOGS) + 1


@pytest.mark.parametrize("name", [f"log_{i + 1:03d}_{n}.ulg" for i, n in enumerate(FLIGHTS)])
def test_a_flight_review_answers_as_python_does(pair, name) -> None:
    diffs = pair.compare_get(f"/api/logs/review?file={name}")
    assert not diffs, "\n".join(diffs[:20])
    assert pair.rs_json("GET", f"/api/logs/review?file={name}")["ok"] is True


@pytest.mark.parametrize("query", [
    "", "?file=", "?file=%20%20", "?file=nothere.ulg", "?file=notes.txt", "?file=..%2F..%2Fetc%2Fpasswd",
    "?file=..", "?file=.", "?file=log_009.bin", "?file=%2Fetc%2Fpasswd",
])
def test_a_refused_review_is_refused_as_python_refuses_it(pair, query) -> None:
    diffs = pair.compare_get(f"/api/logs/review{query}")
    assert not diffs, "\n".join(diffs)


@pytest.mark.parametrize("n", [i for i, name in enumerate(TLOGS) if name != "damaged_tail"])
def test_a_telemetry_review_answers_as_python_does(pair, n) -> None:
    name = f"20260910-1{n}0000-000000.tlog"
    diffs = pair.compare_get(f"/api/logs/tlog-review?file={name}")
    assert not diffs, "\n".join(diffs[:20])


def test_a_damaged_tail_keeps_every_frame_the_file_holds(pair) -> None:
    """Decided difference: the file holds 279 frames with a good checksum. The
    Python reader (corvus_link's pymavlink compatible parser) swallows 18 of them
    behind stray bytes that look like a start of frame; the Rust one keeps all."""
    name = f"20260910-1{TLOGS.index('damaged_tail')}0000-000000.tlog"
    py = harness.http("GET", pair.py.url + f"/api/logs/tlog-review?file={name}")[1]
    rs = pair.rs_json("GET", f"/api/logs/tlog-review?file={name}")
    assert (py["summary"]["frames"], rs["summary"]["frames"]) == (261, 279)
    assert py["ok"] is True and rs["ok"] is True


def test_a_hand_dropped_recording_is_reviewed_too(pair) -> None:
    diffs = pair.compare_get("/api/logs/tlog-review?file=from-a-colleague.tlog")
    assert not diffs, "\n".join(diffs[:20])


@pytest.mark.parametrize("query", ["", "?file=", "?file=nothere.tlog", "?file=..%2Fx.tlog", "?file=notes.txt",
                                   "?file=20260910-100000-000000.ulg"])
def test_a_refused_telemetry_review_is_refused_as_python_refuses_it(pair, query) -> None:
    diffs = pair.compare_get(f"/api/logs/tlog-review{query}")
    assert not diffs, "\n".join(diffs)


def test_the_review_sensitivity_of_the_config_is_read(pair) -> None:
    name = "log_001_clean_quad.ulg"
    strict = pair.rs_json("GET", f"/api/logs/review?file={name}")
    assert strict["ok"] is True
    assert json.dumps(strict, sort_keys=True) == json.dumps(
        harness.http("GET", pair.py.url + f"/api/logs/review?file={name}")[1], sort_keys=True)
    # The same file at the default sensitivity is a different review when the
    # thresholds move anything; at least the config block must be read by both.
    pair.compare_post("/api/config", {"review": {"sensitivity": "relaxed"}})
    diffs = pair.compare_get(f"/api/logs/review?file={name}")
    pair.compare_post("/api/config", {"review": {"sensitivity": "strict"}})
    assert not diffs, "\n".join(diffs[:20])


def test_an_uploaded_log_is_reviewed_as_python_reviews_it(pair, folders) -> None:
    blob = (folders[0] / "log_001_clean_quad.ulg").read_bytes()
    py = _raw_post(pair.py.url, "/api/logs/review/upload?name=" + "flight%20one.ulg", blob)
    rs = _raw_post(pair.rs.url, "/api/logs/review/upload?name=" + "flight%20one.ulg", blob)
    assert py[0] == rs[0] == 200
    assert not harness.diff_json(py[1], rs[1])


@pytest.mark.parametrize("path,body", [
    ("/api/logs/review/upload", b""),
    ("/api/logs/review/upload", b"not a ulog at all"),
    ("/api/logs/review/upload?name=..%2F..%2Fx.ulg", b"ULog\x01\x12\x35\x01"),
    ("/api/logs/review/upload", b"\x00" * 64),
])
def test_a_bad_upload_is_refused_as_python_refuses_it(pair, path, body) -> None:
    py = _raw_post(pair.py.url, path, body)
    rs = _raw_post(pair.rs.url, path, body)
    assert py[0] == rs[0], (py, rs)
    assert not harness.diff_json(py[1], rs[1]), (py, rs)


@pytest.mark.parametrize("path,body", [
    ("/api/logs/refresh", {}),
    ("/api/logs/download", {}),
    ("/api/logs/download", {"ids": []}),
    ("/api/logs/download", {"ids": "1"}),
    ("/api/logs/download", {"ids": [True]}),
    ("/api/logs/download", {"ids": ["1"]}),
    ("/api/logs/download", {"ids": [1, 2.0]}),
    ("/api/logs/erase", {}),
    ("/api/logs/cancel", {}),
])
def test_a_log_job_with_nothing_connected_is_answered_as_python_answers_it(pair, path, body) -> None:
    diffs = pair.compare_post(path, body)
    assert not diffs, "\n".join(diffs)


def test_the_download_folder_is_set_checked_and_stored_as_python_does(pair, folders) -> None:
    other = folders[0].parent / "elsewhere"
    for body in [{}, {"dir": ""}, {"dir": "   "}, {"dir": 5}, {"dir": str(other)}, {"dir": "/proc/nope/x"},
                 {"dir": str(folders[0])}]:
        diffs = pair.compare_post("/api/logs/dir", body)
        assert not diffs, f"{body}: " + "\n".join(diffs)
    diffs = pair.compare_get("/api/config")
    assert not diffs, "\n".join(diffs)
    py = json.loads((pair.py.state_dir / "config.json").read_text(encoding="utf-8"))
    rs = json.loads((pair.rs.state_dir / "config.json").read_text(encoding="utf-8"))
    assert py["log_download_dir"] == rs["log_download_dir"]
    assert os.path.isdir(other)


def test_an_unconfigured_folder_is_the_state_folders_flightlogs() -> None:
    with harness.Pair(seed=harness.standard_seed()) as p:
        diffs = p.compare_get("/api/logs/status")
        assert not diffs, "\n".join(diffs)
        for query in ["?file=a.ulg"]:
            diffs = p.compare_get(f"/api/logs/review{query}") + p.compare_get(f"/api/logs/tlog-review{query}")
            assert not diffs, "\n".join(diffs)
