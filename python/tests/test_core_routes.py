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
    status, body = harness.http("POST", pair.rs.url + "/api/mavlink/scope/clear", {})
    assert (status, body) == (501, {"error": "Not available in this build"})
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


# ---- the vehicle area: the vehicle list, the fence area and the mission files ----

FLEET_VOLATILE = ("connection", "status", "error", "event_seq")


@pytest.fixture(scope="module")
def vehicle_pair():
    with harness.Pair(seed=harness.standard_seed()) as p:
        yield p


def test_the_vehicle_list_starts_as_python_has_it(vehicle_pair) -> None:
    diffs = vehicle_pair.compare_get("/api/fleet", FLEET_VOLATILE)
    assert not diffs, "\n".join(diffs)


@pytest.mark.parametrize("path,body", [
    ("/api/fleet/select", {}),
    ("/api/fleet/select", {"id": 5}),
    ("/api/fleet/select", {"id": "main:1"}),
    ("/api/fleet/select", {"id": "link-9:1"}),
    ("/api/fleet/select", {"id": "main:0"}),
    ("/api/fleet/vehicle", {"id": "main:1", "name": 5}),
    ("/api/fleet/vehicle", {"id": "main:1", "color": 5}),
    ("/api/fleet/vehicle", {"id": "main:1", "color": "red"}),
    ("/api/fleet/vehicle", {"id": "bad", "name": "x"}),
    ("/api/fleet/vehicle", {"id": "main:1", "name": "  Big   Red  "}),
    ("/api/fleet/vehicle", {"id": "main:1", "color": "#abcdef"}),
    ("/api/fleet/vehicle", {"id": "main:1", "name": "", "color": ""}),
    ("/api/fleet/links/add", {}),
    ("/api/fleet/links/add", {"connection": ""}),
    ("/api/fleet/links/add", {"connection": 5}),
    ("/api/fleet/links/add", {"connection": "carrier-pigeon:1"}),
    ("/api/fleet/links/add", {"connection": "serial:"}),
    ("/api/fleet/links/remove", {"id": "main"}),
    ("/api/fleet/links/remove", {"id": "link-7"}),
    ("/api/fleet/links/remove", {}),
    ("/api/fleet/vehicle/remove", {"id": "nonsense"}),
    ("/api/fleet/vehicle/remove", {"id": "main:1"}),
    ("/api/fleet/vehicle/remove", {"id": "link-4:2"}),
    ("/api/fleet/vehicle/remove", {"id": "main:1", "confirm_armed": "yes"}),
])
def test_a_vehicle_list_request_is_answered_as_python_does(vehicle_pair, path, body) -> None:
    diffs = vehicle_pair.compare_post(path, body, FLEET_VOLATILE)
    diffs += vehicle_pair.compare_get("/api/fleet", FLEET_VOLATILE)
    assert not diffs, "\n".join(diffs)


def test_the_vehicle_names_reach_the_config_file_as_python_writes_them(vehicle_pair) -> None:
    import json

    py = json.loads((vehicle_pair.py.state_dir / "config.json").read_text(encoding="utf-8")).get("fleet")
    rs = json.loads((vehicle_pair.rs.state_dir / "config.json").read_text(encoding="utf-8")).get("fleet")
    assert rs == py


GEOFENCE_SQUARE = [[8.5, 47.5], [8.6, 47.5], [8.6, 47.6], [8.5, 47.6]]


@pytest.mark.parametrize("body", [
    {"polygon": GEOFENCE_SQUARE},
    {"polygon": GEOFENCE_SQUARE + [GEOFENCE_SQUARE[0]], "show_on_map": False},
    {"show_on_map": True},
    {"show_on_map": "yes"},
    {"polygon": "x"},
    {"polygon": [[1, 2]]},
    {"polygon": [[0, 0], [2, 2], [2, 0], [0, 2]]},
    {"polygon": [[0, 0], [181, 0], [1, 1]]},
    {"polygon": None},
    {},
])
def test_the_fence_area_is_kept_as_python_keeps_it(vehicle_pair, body) -> None:
    diffs = vehicle_pair.compare_get("/api/geofence")
    diffs += vehicle_pair.compare_post("/api/geofence/save", body)
    diffs += vehicle_pair.compare_get("/api/geofence")
    assert not diffs, "\n".join(diffs)


def test_the_fence_file_is_byte_for_byte_the_pythons(vehicle_pair) -> None:
    vehicle_pair.compare_post("/api/geofence/save", {"polygon": GEOFENCE_SQUARE, "show_on_map": False})
    py = (vehicle_pair.py.state_dir / "geofence.json").read_text(encoding="utf-8")
    rs = (vehicle_pair.rs.state_dir / "geofence.json").read_text(encoding="utf-8")
    assert rs == py


def test_a_fence_that_is_not_connected_is_refused_as_python_refuses_it(vehicle_pair) -> None:
    diffs = vehicle_pair.compare_post("/api/geofence/upload", {})
    diffs += vehicle_pair.compare_post("/api/geofence/clear", {})
    diffs += vehicle_pair.compare_post("/api/geofence/save", {"polygon": []})
    diffs += vehicle_pair.compare_post("/api/geofence/upload", {})
    assert not diffs, "\n".join(diffs)


MISSION_PLAN = {
    "name": "My Plan! v2", "aircraft": "multirotor", "speed": 5,
    "home": {"lat": 47.4, "lon": 8.4},
    "items": [
        {"type": "takeoff", "lat": 47.5, "lon": 8.5, "alt": 20},
        {"type": "waypoint", "lat": 47.501, "lon": 8.502, "alt": 30, "hold": 5, "speed": 8, "name": "Gate A"},
        {"type": "loiter_turns", "lat": 47.502, "lon": 8.505, "alt": 30, "turns": 2, "radius": 30, "direction": -1},
        {"type": "land", "lat": 47.5, "lon": 8.5, "approach_alt": 25},
        {"type": "rtl"},
    ],
}


def test_the_saved_mission_plans_are_kept_as_python_keeps_them(vehicle_pair) -> None:
    p = vehicle_pair
    diffs = p.compare_get("/api/mission/plans", ("modified",))
    for body in (
        {"plan": MISSION_PLAN}, {"plan": MISSION_PLAN, "name": "Second / flight"}, {"plan": {"items": []}},
        {"plan": {"items": [], "name": "../../evil"}}, {"plan": {"items": 5}}, {"plan": None}, {},
        {"plan": {"items": [{"type": "waypoint", "lat": 91, "lon": 0, "alt": 1}]}},
    ):
        diffs += p.compare_post("/api/mission/plans/save", body)
    diffs += p.compare_get("/api/mission/plans", ("modified",))
    for name in ("My Plan v2", "Second flight", "Mission", "evil", "nothing", "", "../x", 5):
        diffs += p.compare_post("/api/mission/plans/load", {"name": name})
    for name in ("Second flight", "Second flight", "", 5):
        diffs += p.compare_post("/api/mission/plans/remove", {"name": name})
    diffs += p.compare_get("/api/mission/plans", ("modified",))
    assert not diffs, "\n".join(diffs)
    py = sorted(f.name for f in (p.py.state_dir / "missions").iterdir())
    rs = sorted(f.name for f in (p.rs.state_dir / "missions").iterdir())
    assert rs == py
    for name in py:
        assert (p.rs.state_dir / "missions" / name).read_text(encoding="utf-8") == \
            (p.py.state_dir / "missions" / name).read_text(encoding="utf-8")


@pytest.mark.parametrize("path,body", [
    ("/api/mission/upload", {"plan": MISSION_PLAN}),
    ("/api/mission/upload", {"plan": MISSION_PLAN, "start": True}),
    ("/api/mission/upload", {"plan": MISSION_PLAN, "start": "yes"}),
    ("/api/mission/upload", {"plan": {"items": []}}),
    ("/api/mission/upload", {"plan": {"items": 5}}),
    ("/api/mission/upload", {"plan": {**MISSION_PLAN, "aircraft": "rover"}}),
    ("/api/mission/upload", {"plan": MISSION_PLAN, "vehicle": "bad"}),
    ("/api/mission/upload", {"plan": MISSION_PLAN, "vehicle": "main:1"}),
    ("/api/mission/upload", {"plan": MISSION_PLAN, "vehicle": "link-5:1"}),
    ("/api/mission/start", {"items": 3}),
    ("/api/mission/start", {"items": 0}),
    ("/api/mission/start", {"items": True}),
    ("/api/mission/start", {"items": 3.0}),
    ("/api/mission/start", {"items": 256}),
    ("/api/mission/start", {}),
    ("/api/mission/clear", {}),
    ("/api/mission/clear", {"vehicle": "main:1"}),
    ("/api/mission/download", {}),
])
def test_a_mission_request_without_a_vehicle_is_refused_as_python_refuses_it(vehicle_pair, path, body) -> None:
    diffs = vehicle_pair.compare_post(path, body)
    assert not diffs, "\n".join(diffs)


# ---- the vehicle link and its commands (routes/vehicle.py), with nothing connected ----

@pytest.mark.parametrize("path", ["/api/mavlink/modes", "/api/mavlink/capabilities", "/api/mavlink/serial-ports",
                                  "/api/mavlink/modes?vehicle=bad", "/api/mavlink/capabilities?vehicle=main:1",
                                  "/api/mavlink/modes?vehicle=link-4:1"])
def test_the_link_reads_are_answered_as_python_answers_them(vehicle_pair, path) -> None:
    diffs = vehicle_pair.compare_get(path)
    assert not diffs, "\n".join(diffs)


def test_what_auto_connect_offers_reads_the_same(vehicle_pair) -> None:
    diffs = vehicle_pair.compare_get("/api/mavlink/auto", ("reason", "winner"))
    assert not diffs, "\n".join(diffs)


COMMAND_BODIES = [
    ("/api/mavlink/arm", {}), ("/api/mavlink/arm", {"arm": False}), ("/api/mavlink/arm", {"arm": "yes"}),
    ("/api/mavlink/arm", {"vehicle": "main:1"}), ("/api/mavlink/arm", {"vehicle": 5}), ("/api/mavlink/arm", {"vehicle": ""}),
    ("/api/mavlink/mode", {}), ("/api/mavlink/mode", {"mode": ""}), ("/api/mavlink/mode", {"mode": 5}),
    ("/api/mavlink/mode", {"mode": "posctl"}),
    ("/api/mavlink/takeoff", {}), ("/api/mavlink/takeoff", {"altitude": 30}), ("/api/mavlink/takeoff", {"altitude": "30"}),
    ("/api/mavlink/takeoff", {"altitude": True}), ("/api/mavlink/takeoff", {"altitude": None}),
    ("/api/mavlink/takeoff", {"altitude": 0.5}), ("/api/mavlink/takeoff", {"altitude": 121}),
    ("/api/mavlink/takeoff", {"altitude": "abc"}), ("/api/mavlink/takeoff", {"altitude": [1]}),
    ("/api/mavlink/takeoff", {"altitude": "inf"}), ("/api/mavlink/takeoff", {"altitude": "nan"}),
    ("/api/mavlink/land", {}), ("/api/mavlink/rtl", {}), ("/api/mavlink/hold", {}), ("/api/mavlink/reboot", {}),
    ("/api/mavlink/shell/send", {}), ("/api/mavlink/shell/send", {"data": ""}), ("/api/mavlink/shell/send", {"data": "ver\n"}),
    ("/api/mavlink/shell/close", {}),
    ("/api/mavlink/manual", {}), ("/api/mavlink/manual", {"x": 2}), ("/api/mavlink/manual", {"y": "a"}),
    ("/api/mavlink/manual", {"z": -0.1}), ("/api/mavlink/manual", {"r": True}), ("/api/mavlink/manual", {"buttons": -1}),
    ("/api/mavlink/manual", {"buttons": 65536}), ("/api/mavlink/manual", {"buttons": 1.0}),
    ("/api/mavlink/manual", {"x": 0.5, "y": -0.5, "z": 0.7, "r": 1, "buttons": 3}),
    ("/api/mavlink/sethome", {}), ("/api/mavlink/sethome", {"lat": 91, "lon": 0}), ("/api/mavlink/sethome", {"lat": 1, "lon": 181}),
    ("/api/mavlink/sethome", {"lat": "1", "lon": 0}), ("/api/mavlink/sethome", {"lat": 47.5, "lon": 8.5}),
    ("/api/mavlink/gotopoints", {}), ("/api/mavlink/gotopoints", {"points": []}), ("/api/mavlink/gotopoints", {"points": 5}),
    ("/api/mavlink/gotopoints", {"points": [5]}), ("/api/mavlink/gotopoints", {"points": [{}]}),
    ("/api/mavlink/gotopoints", {"points": [{"lat": 1, "lon": 2}]}),
    ("/api/mavlink/gotopoints", {"points": [{"lat": 1, "lon": 2, "alt_agl": 0}]}),
    ("/api/mavlink/gotopoints", {"points": [{"lat": 91, "lon": 2, "alt_agl": 10}]}),
    ("/api/mavlink/gotopoints", {"points": [{"lat": 1, "lon": 181, "alt_agl": 10}]}),
    ("/api/mavlink/gotopoints", {"points": [{"lat": 1, "lon": 2, "alt_agl": 10}]}),
    ("/api/mavlink/gotopoints", {"points": [{"lat": 1, "lon": 2, "alt_agl": 10}] * 256}),
    ("/api/mavlink/gotopoints", {"points": [{"lat": True, "lon": 2, "alt_agl": 10}]}),
    ("/api/mavlink/command", {}), ("/api/mavlink/command", {"command": "NOPE"}), ("/api/mavlink/command", {"command": True}),
    ("/api/mavlink/command", {"command": 70000}), ("/api/mavlink/command", {"command": "do_change_speed", "params": [1, 5]}),
    ("/api/mavlink/command", {"command": "MAV_CMD_DO_CHANGE_SPEED", "params": [1, None, -1]}),
    ("/api/mavlink/command", {"command": 178, "params": [1] * 8}), ("/api/mavlink/command", {"command": 178, "params": "x"}),
    ("/api/mavlink/command", {"command": 178, "params": [True]}), ("/api/mavlink/command", {"command": 178, "lat": 1}),
    ("/api/mavlink/command", {"command": "DO_REPOSITION", "lat": 1, "lon": 2}),
    ("/api/mavlink/command", {"command": "DO_REPOSITION", "lat": 1}),
    ("/api/mavlink/command", {"command": "DO_REPOSITION", "lat": 91, "lon": 2}),
    ("/api/mavlink/command", {"command": "DO_REPOSITION", "lat": 1, "lon": 2, "frame": "x"}),
    ("/api/mavlink/command", {"command": "DO_REPOSITION", "lat": 1, "lon": 2, "frame": "amsl", "alt": 100}),
    ("/api/mavlink/command", {"command": "DO_REPOSITION", "params": [1, 2, 3, 4, 5]}),
    ("/api/mavlink/command", {"command": "DO_ORBIT", "lat": 1, "lon": 2, "alt": "x"}),
    ("/api/mavlink/velocity", {}), ("/api/mavlink/velocity", {"vx": 31}), ("/api/mavlink/velocity", {"vy": "a"}),
    ("/api/mavlink/velocity", {"yaw_rate": 181}), ("/api/mavlink/velocity", {"yaw_rate": "a"}),
    ("/api/mavlink/velocity", {"frame": "x"}), ("/api/mavlink/velocity", {"vx": 1, "vy": 1, "vz": -1, "yaw_rate": 10, "frame": "body"}),
]


@pytest.mark.parametrize("path,body", COMMAND_BODIES)
def test_a_flight_command_with_nothing_connected_is_answered_as_python_answers_it(vehicle_pair, path, body) -> None:
    diffs = vehicle_pair.compare_post(path, body)
    assert not diffs, "\n".join(diffs)


@pytest.mark.parametrize("body", [
    {"connection": ""}, {"connection": 5}, {"connection": "carrier-pigeon:1"}, {"connection": "serial:"},
    {"connection": "serial:/dev/x:0"},
])
def test_a_refused_connect_is_refused_as_python_refuses_it(vehicle_pair, body) -> None:
    diffs = vehicle_pair.compare_post("/api/mavlink/connect", body)
    assert not diffs, "\n".join(diffs)


def test_a_manual_connect_and_a_disconnect_leave_auto_connect_as_python_leaves_it() -> None:
    with harness.Pair(seed=harness.standard_seed()) as p:
        diffs = p.compare_post("/api/mavlink/connect", {"connection": "udpout:127.0.0.1:9"})
        diffs += p.compare_get("/api/mavlink/auto")
        diffs += p.compare_post("/api/mavlink/disconnect", {})
        diffs += p.compare_get("/api/mavlink/auto")
        diffs += p.compare_post("/api/fleet/links/add", {"connection": "udpout:127.0.0.1:9"})
        diffs += p.compare_post("/api/mavlink/connect", {"connection": "udpout:127.0.0.1:10"}, FLEET_VOLATILE)
        diffs += p.compare_get("/api/fleet", FLEET_VOLATILE)
        assert not diffs, "\n".join(diffs)


@pytest.mark.parametrize("path,body", [
    ("/api/motors/test", {}), ("/api/motors/test", {"motor": True}), ("/api/motors/test", {"motor": "1"}),
    ("/api/motors/test", {"motor": 1, "throttle": "x"}), ("/api/motors/test", {"motor": 1, "duration": None}),
    ("/api/motors/test", {"motor": 1, "throttle": 101}), ("/api/motors/test", {"motor": 1, "throttle": -1}),
    ("/api/motors/test", {"motor": 1}), ("/api/motors/test", {"motor": 1, "throttle": 20.5, "duration": 3}),
    ("/api/motors/test/stop", {}),
    ("/api/rangefinder/stream", {}), ("/api/rangefinder/stream", {"enabled": "yes"}), ("/api/rangefinder/stream", {"enabled": True}),
    ("/api/rangefinder/stream", {"enabled": False}),
    ("/api/rc/stream", {}), ("/api/rc/stream", {"enabled": 1}), ("/api/rc/stream", {"enabled": True, "rate_hz": "x"}),
    ("/api/rc/stream", {"enabled": True, "rate_hz": True}), ("/api/rc/stream", {"enabled": True, "rate_hz": 0}),
    ("/api/rc/stream", {"enabled": True, "rate_hz": 51}), ("/api/rc/stream", {"enabled": True, "rate_hz": 2.5}),
    ("/api/rc/stream", {"enabled": True, "rate_hz": 10.0}), ("/api/rc/stream", {"enabled": True}),
    ("/api/rc/stream", {"enabled": False, "rate_hz": 5}),
])
def test_an_actuator_or_stream_request_with_nothing_connected_is_answered_as_python_answers_it(vehicle_pair, path, body) -> None:
    diffs = vehicle_pair.compare_post(path, body)
    assert not diffs, "\n".join(diffs)


# ---- the setup pages and the parameter routes, with nothing connected ----

@pytest.mark.parametrize("path", [
    "/api/motors", "/api/motors?fresh=1", "/api/rc", "/api/params", "/api/params/upload/result",
    "/api/params/export/target", "/api/first-setup", "/api/params/metadata", "/api/params/metadata/cache",
])
def test_a_setup_or_parameter_read_with_nothing_connected_is_answered_as_python_answers_it(vehicle_pair, path) -> None:
    diffs = vehicle_pair.compare_get(path, ("dir", "filename"))
    assert not diffs, "\n".join(diffs)


@pytest.mark.parametrize("path,body", [
    ("/api/motors/assign", {}), ("/api/motors/assign", {"motor": True}), ("/api/motors/assign", {"motor": 0}),
    ("/api/motors/assign", {"motor": 17}), ("/api/motors/assign", {"motor": 1}),
    ("/api/motors/assign", {"motor": 1, "bank": 5, "pin": 1}), ("/api/motors/assign", {"motor": 1, "bank": "MAIN"}),
    ("/api/motors/assign", {"motor": 1, "bank": "MAIN", "pin": True}),
    ("/api/motors/assign", {"motor": 1, "bank": "NOPE", "pin": 1}),
    ("/api/motors/assign", {"motor": 1, "bank": "MAIN", "pin": 99}),
    ("/api/motors/assign", {"motor": 1, "bank": "MAIN", "pin": 3}), ("/api/motors/assign", {"motor": 2, "output": None}),
    ("/api/rc/calibrate", {}), ("/api/rc/calibrate", {"count": "x"}), ("/api/rc/calibrate", {"count": True}),
    ("/api/rc/calibrate", {"channels": []}),
    ("/api/calibrate", {}), ("/api/calibrate", {"type": 5}), ("/api/calibrate", {"type": "nope"}),
    ("/api/calibrate", {"type": "GYRO"}), ("/api/calibrate", {"type": "accel"}),
    ("/api/calibrate/position", {}), ("/api/calibrate/position", {"position": "up"}),
    ("/api/calibrate/position", {"position": " Level "}), ("/api/calibrate/cancel", {}),
    ("/api/autotune", {}), ("/api/autotune", {"axis": "roll"}), ("/api/autotune", {"axis": "all", "enabled": "no"}),
    ("/api/autotune", {"axis": " ALL "}), ("/api/autotune", {"axis": "all", "enabled": False}),
    ("/api/vibration/stream", {}), ("/api/vibration/stream", {"enabled": 1}),
    ("/api/vibration/stream", {"enabled": True, "rate_hz": "x"}), ("/api/vibration/stream", {"enabled": True, "rate_hz": True}),
    ("/api/vibration/stream", {"enabled": True, "rate_hz": 0}), ("/api/vibration/stream", {"enabled": True, "rate_hz": 2.5}),
    ("/api/vibration/stream", {"enabled": True}), ("/api/vibration/stream", {"enabled": False, "rate_hz": 50}),
    ("/api/tuning/stream", {}), ("/api/tuning/stream", {"enabled": True, "rate_hz": 51}),
    ("/api/tuning/stream", {"enabled": True}), ("/api/tuning/stream", {"enabled": False, "rate_hz": 5}),
    ("/api/warnings/clear", {}),
    ("/api/params/download", {}),
    ("/api/params/set", {}), ("/api/params/set", {"name": ""}), ("/api/params/set", {"name": "A", "value": True}),
    ("/api/params/set", {"name": "A", "value": "1"}), ("/api/params/set", {"name": "A", "value": 1}),
    ("/api/params/verify", {}), ("/api/params/verify", {"params": 5}), ("/api/params/verify", {"names": [1]}),
    ("/api/params/verify", {"names": [""]}), ("/api/params/verify", {"params": [5]}),
    ("/api/params/verify", {"params": [{"name": ""}]}), ("/api/params/verify", {"params": [{"name": "A"}]}),
    ("/api/params/verify", {"params": [{"name": "A", "value": True}]}),
    ("/api/params/verify", {"params": [{"name": "A", "value": 1}], "names": ["B"]}),
    ("/api/params/upload", {}), ("/api/params/upload", {"params": []}), ("/api/params/upload", {"params": [5]}),
    ("/api/params/upload", {"params": [{"name": 1, "value": 1}]}), ("/api/params/upload", {"params": [{"name": "A"}]}),
    ("/api/params/upload", {"params": [{"name": "A", "value": 1}]}),
    ("/api/params/metadata", {}), ("/api/params/metadata/cache/clear", {}),
    ("/api/first-setup/airframe", {}), ("/api/first-setup/airframe", {"preset": True}),
    ("/api/first-setup/airframe", {"preset": "4001"}), ("/api/first-setup/airframe", {"preset": 4001}),
    ("/api/params/export", {}), ("/api/params/export", {"params": []}), ("/api/params/export", {"params": [{"name": "A", "value": 1}], "format": "x"}),
    ("/api/params/export", {"params": [5]}), ("/api/params/export", {"params": [{"name": "A", "value": True}]}),
])
def test_a_setup_or_parameter_write_with_nothing_connected_is_answered_as_python_answers_it(vehicle_pair, path, body) -> None:
    diffs = vehicle_pair.compare_post(path, body)
    assert not diffs, "\n".join(diffs)


# ---- return profile, live messages, forwarding and Remote ID ----

@pytest.mark.parametrize("path", [
    "/api/mission/return", "/api/mavlink/scope", "/api/mavlink/scope?select=1:1:HEARTBEAT", "/api/remoteid",
    "/api/remoteid/status", "/api/forwarding",
])
def test_the_return_scope_remote_id_and_forwarding_reads_answer_as_python_answers_them(vehicle_pair, path) -> None:
    diffs = vehicle_pair.compare_get(path, ("since",))
    assert not diffs, "\n".join(diffs)


def test_forwarding_is_set_refused_and_kept_as_python_does_it() -> None:
    import json

    bodies = [
        {"enabled": "yes"}, {"allow_commands": 1}, {"port": 0}, {"port": 70000}, {"port": True}, {"port": "x"},
        {"listen_port": -1}, {"listen_port": 65536}, {"endpoints": "a"}, {"endpoints": [1]},
        {"enabled": False, "allow_commands": True, "host": " 10.0.0.5 ", "port": 14560, "listen_host": "  ",
         "endpoints": ["a:1", " ", "b"]},
        {"enabled": False},
        {"enabled": True, "listen_port": 0, "port": 14570, "endpoints": []},
        {"enabled": False, "allow_commands": False},
    ]
    with harness.Pair(seed=harness.standard_seed()) as p:
        diffs = p.compare_get("/api/forwarding")
        for body in bodies:
            diffs += p.compare_post("/api/forwarding", body, ("listen_port", "listen_host", "notice", "targets", "peers"))
            diffs += p.compare_get("/api/forwarding", ("listen_port", "notice", "targets", "peers"))
        assert not diffs, "\n".join(diffs)
        py = json.loads((p.py.state_dir / "config.json").read_text())["forwarding"]
        rs = json.loads((p.rs.state_dir / "config.json").read_text())["forwarding"]
        assert py == rs, (py, rs)


def test_the_scope_clear_answers_as_python_answers_it(vehicle_pair) -> None:
    diffs = vehicle_pair.compare_post("/api/mavlink/scope/clear", {})
    assert not diffs, "\n".join(diffs)


@pytest.mark.parametrize("identity", [
    {"enabled": True, "region": "eu", "basic_id": {"id_type": 1, "ua_type": 2, "uas_id": "1234ABCDE"},
     "operator_id": {"operator_id": "FIN87astrdge12k8xyz"}, "system": {"classification_type": 1, "category_eu": 1}},
    {"region": "faa", "basic_id": {"id_type": 2, "uas_id": "1234AB"}, "system": {"operator_location_type": 0}},
    {"region": "eu", "basic_id": {"id_type": 1, "uas_id": "ab-OI"}, "operator_id": {"operator_id": "FIN-short"},
     "system": {"operator_location_type": 2}},
    {"region": "faa", "basic_id": {"id_type": 1, "uas_id": "ABCDX12345678901234567"},
     "system": {"operator_location_type": 1}},
    {"region": "eu", "basic_id": {"id_type": 1, "uas_id": "ABCD4WXYZ"}, "operator_id": {"operator_id": "FIN87astrdge12k8"},
     "system": {"classification_type": 1, "category_eu": 0}},
])
def test_the_remote_id_page_judges_an_identity_as_python_does(identity) -> None:
    seed = harness.standard_seed()
    seed["config"]["remote_id"] = identity
    with harness.Pair(seed=seed) as p:
        diffs = p.compare_get("/api/remoteid") + p.compare_get("/api/remoteid/status")
        assert not diffs, "\n".join(diffs)
