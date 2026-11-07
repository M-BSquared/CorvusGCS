"""The Rust core held to the Python server.

Starts ``serve.py`` and the Rust ``corvus-core`` binary on identical state
folders and compares what they answer (``tools/route_harness.py``). These are
the tests of the foundation's routes; each area of the Rust port adds its own
here, with the same helper. Skipped when the binary is not built:

    cargo build -p corvus_core --bin corvus-core
"""
from __future__ import annotations

import os
import pathlib
import signal
import subprocess
import sys
import time

import pytest

pytest.importorskip("corvus_link")
sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent.parent / "tools"))

import route_harness as harness  # noqa: E402

pytestmark = pytest.mark.skipif(harness.core_binary() is None, reason="corvus-core is not built")


@pytest.fixture(scope="module")
def pair():
    with harness.Pair(seed=harness.standard_seed()) as p:
        yield p


def test_every_foundation_get_route_answers_as_python_does(pair) -> None:
    diffs = harness.run_foundation(pair)
    assert not diffs, "\n".join(diffs)


@pytest.mark.parametrize("name,body", [
    ("ints_and_strings", {"http_port": "9001", "tlog_dir": "/x", "missions_dir": ""}),
    ("merged_ui", {"ui": {"scale": 2, "units": {"speed": "kn"}}}),
    ("merged_controls_map", {"controls": {"arrow_keys": True}, "map": {"three_d": "full"}}),
    ("replaced_theme", {"theme": {"name": "pink"}}),
    ("language_cleared", {"language": ""}),
    ("ssh_list", {"ssh_connections": [{"name": "z", "host": "q", "port": "2222"}]}),
    ("updates_skip", {"updates": {"skipped": "v2026.10.01", "check": False}}),
    ("owned_elsewhere_and_unknown", {"forwarding": {"enabled": True}, "surprise": 1}),
    ("port_out_of_range", {"http_port": 70000}),
    ("port_bool", {"http_port": True}),
    ("theme_not_an_object", {"theme": "pink"}),
    ("language_bad", {"language": "xx_yy"}),
    ("connection_empty", {"mavlink_connection": ""}),
    ("empty", {}),
])
def test_a_config_update_answers_and_changes_the_config_as_python_does(pair, name, body) -> None:
    diffs = pair.compare_post("/api/config", body)
    diffs += pair.compare_get("/api/config")
    assert not diffs, "\n".join(diffs)


def test_the_config_file_is_byte_for_byte_the_pythons_after_those_updates(pair) -> None:
    py = (pair.py.state_dir / "config.json").read_text(encoding="utf-8")
    rs = (pair.rs.state_dir / "config.json").read_text(encoding="utf-8")
    assert rs == py
    if os.name == "posix":
        assert (pair.rs.state_dir / "config.json").stat().st_mode & 0o777 == 0o600


def test_a_body_that_is_not_an_object_is_refused_the_same_way(pair) -> None:
    for raw in ([1], "text", 7):
        (ps, pb), (rs, rb) = pair.both("POST", "/api/config", raw)
        assert (ps, pb) == (rs, rb) == (400, {"error": "json payload must be an object"})


def test_the_first_start_is_pending_until_it_is_finished() -> None:
    with harness.Pair(seed=None) as fresh:
        assert not fresh.compare_get("/api/welcome")
        assert fresh.rs_json("GET", "/api/welcome") == {"pending": True}
        assert not fresh.compare_post("/api/welcome", {})
        assert fresh.rs_json("GET", "/api/welcome") == {"pending": False}
        assert (fresh.rs.state_dir / "config.json").read_text() == (fresh.py.state_dir / "config.json").read_text()


def test_an_area_that_is_not_ported_says_so_instead_of_404(pair) -> None:
    status, body = harness.http("POST", pair.rs.url + "/api/mavlink/arm", {})
    assert (status, body) == (501, {"error": "Not available in this build"})
    status, body = harness.http("GET", pair.rs.url + "/api/tiles/esri/1/2/3.png")
    assert status == 501
    assert harness.http("GET", pair.rs.url + "/api/no-such-route")[0] == 404
    assert harness.http("GET", pair.py.url + "/api/no-such-route")[0] == 404


def test_a_request_for_another_host_is_refused_like_python_refuses_it(pair) -> None:
    import http.client

    for server in (pair.py, pair.rs):
        conn = http.client.HTTPConnection("127.0.0.1", server.port, timeout=10)
        conn.request("GET", "/api/version", headers={"Host": "evil.example"})
        resp = conn.getresponse()
        assert resp.status == 403, server.name
        assert b"host not allowed" in resp.read()
        conn.close()


def test_the_telemetry_stream_starts_with_a_state_event(pair) -> None:
    import http.client

    first = {}
    for server in (pair.py, pair.rs):
        conn = http.client.HTTPConnection("127.0.0.1", server.port, timeout=10)
        conn.request("GET", "/api/telemetry")
        resp = conn.getresponse()
        assert resp.status == 200 and resp.getheader("Content-Type") == "text/event-stream", server.name
        line = resp.fp.readline().decode()
        data = resp.fp.readline().decode()
        assert line == "event: state\n" and data.startswith("data: {"), (server.name, line, data[:40])
        first[server.name] = data
        conn.close()
    import json

    keys = {name: list(json.loads(d[6:])) for name, d in first.items()}
    assert keys["rust"] == keys["python"], "same fields in the same order"


def test_the_instance_lock_is_the_same_file_python_holds(pair) -> None:
    env = dict(os.environ)
    env.pop("CORVUS_ALLOW_MULTI", None)
    done = subprocess.run([str(harness.core_binary()), "--port", "0", "--state-dir", str(pair.py.state_dir)],
                          env=env, capture_output=True, text=True, timeout=30)
    assert done.returncode != 0
    assert "CORVUS GCS is already running" in done.stderr
    assert "CORVUS_ALLOW_MULTI" in done.stderr


def test_the_core_stops_on_sigterm_with_exit_zero(tmp_path) -> None:
    proc = subprocess.Popen([str(harness.core_binary()), "--port", "0", "--state-dir", str(tmp_path / ".corvus")],
                            stdout=subprocess.PIPE, text=True)
    try:
        assert proc.stdout.readline().startswith("listening http://127.0.0.1:")
        began = time.monotonic()
        proc.send_signal(signal.SIGTERM)
        assert proc.wait(timeout=15) == 0
        assert time.monotonic() - began < 5
    finally:
        if proc.poll() is None:
            proc.kill()
