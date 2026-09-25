"""Parameter export — filename generation, path safety, and the write itself.

The export writes the file SERVER-side rather than handing the browser a blob
download. That is not a style choice: the desktop build runs the UI inside
QtWebEngine, which drops an ``<a download>`` unless the host application
implements a download handler, so the previous blob export produced no file at
all there. Writing it here behaves identically in the desktop app and in a
browser, lands it in a folder the operator chose, and can report exactly where
it went — which is what these tests pin.

Hermetic: every write goes to ``tmp_path``; no network, no real vehicle.
"""
from __future__ import annotations

import http.client
import json
import os
import threading

import pytest

pytest.importorskip("pymavlink")
pytest.importorskip("paramiko")

from corvus.config import CorvusConfig  # noqa: E402
from corvus.server import (  # noqa: E402
    CorvusHandler,
    CorvusServer,
    _default_params_filename,
    _params_export_dir,
    _safe_filename,
)
from corvus.paths import corvus_path  # noqa: E402
from corvus.version import get_version  # noqa: E402

from conftest import posix_permissions


# ---------------------------------------------------------------------------
# 1. Where the file goes
# ---------------------------------------------------------------------------

def test_export_dir_defaults_to_the_corvus_params_folder() -> None:
    assert _params_export_dir(CorvusConfig()) == corvus_path("params")


def test_export_dir_honours_a_pinned_params_dir() -> None:
    cfg = CorvusConfig(params_dir="~/flights/params")
    assert _params_export_dir(cfg) == os.path.expanduser("~/flights/params")


def test_export_dir_treats_whitespace_as_unset() -> None:
    """A field the operator cleared must fall back, not write to a blank path."""
    assert _params_export_dir(CorvusConfig(params_dir="   ")) == \
        corvus_path("params")


# ---------------------------------------------------------------------------
# 2. The generated filename
# ---------------------------------------------------------------------------

def test_default_filename_is_readable_and_carries_the_vehicle() -> None:
    """A folder of exports has to be scannable by eye, and "which airframe was
    this?" is the first question asked of an old parameter file — hence a
    readable date and a vehicle tag, not the old epoch-milliseconds name."""
    name = _default_params_filename("PX4 QUADROTOR")
    assert name.startswith("corvus-params_px4-quadrotor_")
    assert name.endswith(".json")
    # corvus-params_<tag>_<YYYY-MM-DD>_<HH-MM>.json
    stamp = name[: -len(".json")].split("_")[-2:]
    assert len(stamp[0]) == 10 and stamp[0].count("-") == 2
    assert len(stamp[1]) == 5 and stamp[1].count("-") == 1


def test_default_filename_without_a_vehicle_tag() -> None:
    name = _default_params_filename("")
    assert name.startswith("corvus-params_") and name.endswith(".json")
    assert "__" not in name, "a missing tag must not leave an empty segment"


# ---------------------------------------------------------------------------
# 3. Filename safety
# ---------------------------------------------------------------------------

@pytest.mark.parametrize("raw,expected", [
    ("params", "params.json"),
    ("params.json", "params.json"),
    ("params.JSON", "params.json"),            # suffix normalized
    ("before maiden flight", "before maiden flight.json"),
    ("../../etc/passwd", "passwd.json"),       # directory components stripped
    ("/abs/path/x.json", "x.json"),
    ('we"ird:name*?', "weirdname.json"),       # reserved characters dropped
    ("", "FALLBACK.json"),
    ("   ", "FALLBACK.json"),
    (".", "FALLBACK.json"),
    ("..", "FALLBACK.json"),
    (None, "FALLBACK.json"),
    (42, "FALLBACK.json"),
])
def test_safe_filename(raw, expected) -> None:
    assert _safe_filename(raw, "FALLBACK.json") == expected


def test_safe_filename_truncates_the_stem_not_the_suffix() -> None:
    """Truncating after appending would chop ".json" off a long name and leave
    an unrecognizable, un-importable file."""
    out = _safe_filename("x" * 300, "FALLBACK.json")
    assert out.endswith(".json")
    assert len(out) <= 120


# ---------------------------------------------------------------------------
# 4. The endpoint
# ---------------------------------------------------------------------------

@pytest.fixture
def export_server(tmp_path):
    """Live CorvusServer whose export dir is redirected into tmp_path."""
    cfg = CorvusConfig(params_dir=str(tmp_path / "exports"))
    saved = (
        CorvusHandler.store, CorvusHandler.mavlink, CorvusHandler.ssh,
        CorvusHandler.config, CorvusHandler.config_path,
    )
    CorvusHandler.store = None
    CorvusHandler.mavlink = None
    CorvusHandler.ssh = None
    CorvusHandler.config = cfg
    CorvusHandler.config_path = str(tmp_path / "config.json")

    server = CorvusServer(("127.0.0.1", 0), CorvusHandler)
    thread = threading.Thread(
        target=server.serve_forever, name="corvus-test-export", daemon=True,
        kwargs={"poll_interval": 0.05},
    )
    thread.start()
    try:
        yield server, tmp_path / "exports"
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=2)
        (
            CorvusHandler.store, CorvusHandler.mavlink, CorvusHandler.ssh,
            CorvusHandler.config, CorvusHandler.config_path,
        ) = saved


def _req(server, method: str, path: str, payload: dict | None = None):
    conn = http.client.HTTPConnection("127.0.0.1", server.server_address[1], timeout=5)
    if payload is None:
        conn.request(method, path)
    else:
        conn.request(method, path, json.dumps(payload), {"Content-Type": "application/json"})
    resp = conn.getresponse()
    body = resp.read()
    conn.close()
    return resp.status, json.loads(body) if body else {}


_PARAMS = [
    {"name": "MC_ROLL_P", "value": 6.5, "type": 9},
    {"name": "FW_ACRO_LIM", "value": 1, "type": 9},
]


def test_export_target_reports_the_dir_and_a_default_filename(export_server) -> None:
    """The dialog prefills from this, so the operator sees the real path before
    committing instead of hunting for a file afterwards."""
    server, exports = export_server
    status, data = _req(server, "GET", "/api/params/export/target")
    assert status == 200
    assert data["dir"] == str(exports)
    assert data["filename"].startswith("corvus-params_")


def test_export_writes_to_the_configured_dir(export_server) -> None:
    server, exports = export_server
    status, res = _req(server, "POST", "/api/params/export", {"params": _PARAMS})
    assert status == 200 and res["ok"] is True
    assert res["dir"] == str(exports)
    assert res["param_count"] == 2
    # The directory is created on demand — the operator should not have to.
    assert os.path.isfile(res["path"])


def test_exported_file_content(export_server) -> None:
    server, _ = export_server
    _, res = _req(server, "POST", "/api/params/export",
                  {"filename": "set.json", "params": _PARAMS})
    with open(res["path"], encoding="utf-8") as f:
        doc = json.load(f)
    assert doc["product"] == "Corvus GCS"
    # The backend stamps the version so there is exactly one source of truth
    # for it; the frontend must not put one in the payload.
    assert doc["version"] == get_version()
    assert doc["exported_at"].endswith("Z")
    assert doc["param_count"] == 2
    assert [p["name"] for p in doc["params"]] == ["MC_ROLL_P", "FW_ACRO_LIM"]


def test_export_honours_an_explicit_dir_and_filename(export_server, tmp_path) -> None:
    server, _ = export_server
    target = tmp_path / "usb" / "flights"
    status, res = _req(server, "POST", "/api/params/export", {
        "dir": str(target), "filename": "before-maiden-flight", "params": _PARAMS,
    })
    assert status == 200
    assert res["filename"] == "before-maiden-flight.json"
    assert res["path"] == str(target / "before-maiden-flight.json")
    assert os.path.isfile(res["path"])


def test_export_cannot_escape_the_target_dir(export_server, tmp_path) -> None:
    """A filename with directory components is reduced to its basename, so a
    traversal attempt lands inside the chosen folder."""
    server, _ = export_server
    target = tmp_path / "usb"
    _, res = _req(server, "POST", "/api/params/export", {
        "dir": str(target), "filename": "../../escaped.json", "params": _PARAMS,
    })
    assert res["path"] == str(target / "escaped.json")
    assert os.path.isfile(target / "escaped.json")
    assert not os.path.exists(tmp_path.parent / "escaped.json")


def test_export_blank_dir_falls_back_to_the_configured_one(export_server) -> None:
    server, exports = export_server
    _, res = _req(server, "POST", "/api/params/export", {"dir": "   ", "params": _PARAMS})
    assert res["dir"] == str(exports)


def test_export_rejects_an_empty_param_set(export_server) -> None:
    server, _ = export_server
    for payload in ({"params": []}, {"params": "nope"}, {}):
        status, res = _req(server, "POST", "/api/params/export", payload)
        assert status == 400
        assert res["ok"] is False


@posix_permissions
def test_export_reports_an_unwritable_dir_instead_of_crashing(export_server, tmp_path) -> None:
    """The operator needs the reason so they can pick another folder and retry."""
    server, _ = export_server
    blocked = tmp_path / "blocked"
    blocked.mkdir()
    blocked.chmod(0o500)   # r-x: cannot create files inside
    try:
        status, res = _req(server, "POST", "/api/params/export",
                           {"dir": str(blocked), "params": _PARAMS})
        assert status == 400
        assert res["ok"] is False
        assert str(blocked) in res["error"]
    finally:
        blocked.chmod(0o700)


def test_export_leaves_no_temp_file_behind_on_failure(export_server, tmp_path) -> None:
    """A half-written parameter file is worse than none — it looks importable."""
    server, _ = export_server
    target = tmp_path / "temped"
    target.mkdir()
    # A value json.dump cannot serialize fails mid-write, after the temp file
    # has been created.
    status, _ = _req(server, "POST", "/api/params/export",
                     {"dir": str(target), "params": [{"name": "X", "value": float("inf")}]})
    leftovers = [p for p in os.listdir(target) if p.endswith(".tmp")]
    assert leftovers == [], f"temp files left behind: {leftovers}"


def test_export_overwrites_an_existing_file_atomically(export_server) -> None:
    server, _ = export_server
    _, first = _req(server, "POST", "/api/params/export",
                    {"filename": "set.json", "params": _PARAMS})
    _, second = _req(server, "POST", "/api/params/export",
                     {"filename": "set.json", "params": _PARAMS[:1]})
    assert first["path"] == second["path"]
    with open(second["path"], encoding="utf-8") as f:
        assert json.load(f)["param_count"] == 1


# ---------------------------------------------------------------------------
# 5. The file formats other ground stations read
# ---------------------------------------------------------------------------

def test_export_target_offers_the_formats_and_picks_qgc_without_a_vehicle(export_server) -> None:
    """PX4 is the default stack, and QGroundControl's .params is its file."""
    server, _ = export_server
    _, data = _req(server, "GET", "/api/params/export/target")
    assert data["format"] == "qgc"
    assert data["filename"].endswith(".params")
    assert {f["id"]: f["suffix"] for f in data["formats"]} == {
        "qgc": ".params", "mission-planner": ".param", "json": ".json",
    }


def test_export_writes_a_qgroundcontrol_params_file(export_server) -> None:
    server, _ = export_server
    params = [
        {"name": "MC_ROLL_P", "value": 6.5, "type": 9},
        {"name": "MPC_XY_VEL_MAX", "value": 0.10000000149011612, "type": 9},
        {"name": "COM_RC_IN_MODE", "value": 3, "type": 6},
    ]
    status, res = _req(server, "POST", "/api/params/export",
                       {"filename": "quad", "format": "qgc", "params": params})
    assert status == 200 and res["filename"] == "quad.params"
    with open(res["path"], encoding="utf-8") as f:
        text = f.read()
    assert text.startswith("# Onboard parameters for Vehicle 1\n")
    rows = [line.split("\t") for line in text.splitlines() if not line.startswith("#")]
    assert rows == [
        ["1", "1", "COM_RC_IN_MODE", "3", "6"],
        ["1", "1", "MC_ROLL_P", "6.5", "9"],
        ["1", "1", "MPC_XY_VEL_MAX", "0.1", "9"],
    ], "tab separated, sorted, float32 values written as typed, the vehicle's own type"


def test_export_writes_a_mission_planner_param_file(export_server) -> None:
    server, _ = export_server
    status, res = _req(server, "POST", "/api/params/export", {
        "filename": "plane.param", "format": "mission-planner",
        "params": [{"name": "ARMING_CHECK", "value": 1, "type": 6},
                   {"name": "ATC_RAT_RLL_P", "value": 0.13500000536441803, "type": 9}],
    })
    assert status == 200 and res["filename"] == "plane.param"
    with open(res["path"], encoding="utf-8") as f:
        lines = [line for line in f.read().splitlines() if not line.startswith("#")]
    assert lines == ["ARMING_CHECK,1", "ATC_RAT_RLL_P,0.135"]


def test_export_refuses_an_unknown_format(export_server) -> None:
    server, _ = export_server
    status, res = _req(server, "POST", "/api/params/export",
                       {"format": "xml", "params": _PARAMS})
    assert status == 400 and "format" in res["error"]


def test_export_refuses_a_parameter_without_a_number(export_server) -> None:
    server, _ = export_server
    status, _ = _req(server, "POST", "/api/params/export",
                     {"format": "qgc", "params": [{"name": "X", "value": "1"}]})
    assert status == 400


@pytest.mark.parametrize("value,ptype,expected", [
    (0.10000000149011612, 9, "0.1"),
    (6.5, 9, "6.5"),
    (1.0, 9, "1"),
    (-3, 6, "-3"),
    (1e-07, 9, "1e-07"),
    (123456.78125, 9, "123456.78"),
])
def test_values_are_written_as_the_shortest_exact_float32(value, ptype, expected) -> None:
    from corvus.param_files import format_param_value
    assert format_param_value(value, ptype) == expected
