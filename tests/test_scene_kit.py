"""The screenshot scenes stay in step with the product they photograph.

``tools/scene.py`` regenerates the README's images by putting a simulated PX4
on the wire and driving the real interface. That only works while the scenes
and the application agree about three things: which parameters a page reads,
what the calibration command selects, and which selectors the recipe clicks.
All three are in the product and move with it, so each one is pinned here —
a page that silently renders empty is a screenshot nobody notices is wrong
until it is in the README.

The suite stays importable in a minimal environment the same way the rest of
it does: the scene kit pulls pymavlink, so it is skipped rather than imported
when that is absent.
"""
from __future__ import annotations

import json
import sys
from pathlib import Path

import pytest

pytest.importorskip("pymavlink")

# tools/ is a scripts directory, not an installed package: the scene kit is
# imported the way scene.py imports it rather than by adding it to the suite's
# global path, so nothing else in the tests can accidentally depend on it.
TOOLS = Path(__file__).resolve().parents[1] / "tools"
if str(TOOLS) not in sys.path:
    sys.path.insert(0, str(TOOLS))

from scene_kit import flight, library, params, recipe, vehicle  # noqa: E402

from corvus import motor_config, safety_config, tuning_config  # noqa: E402
from corvus.mavlink_bridge import MavlinkBridge  # noqa: E402


# --------------------------------------------------------------------------
# The parameter table renders the pages it is meant to render
# --------------------------------------------------------------------------


@pytest.mark.parametrize("scene_id", sorted(library.SCENES))
def test_every_scene_builds(scene_id: str) -> None:
    scene = library.get(scene_id)
    table = scene.build_params()
    model = scene.build_model()

    assert table, "a scene with no parameters renders every page as 'not connected'"
    assert model.duration > 0, "a path with no length never moves the aircraft"
    assert len(scene.build_logs()) == scene.logs


@pytest.mark.parametrize("scene_id", sorted(library.SCENES))
def test_configuration_pages_render_from_a_scene(scene_id: str) -> None:
    """Every page a scene can open has content when built from its parameters."""
    table = library.get(scene_id).build_params()

    motors = motor_config.build(table)
    assert motors["rotor_count"] >= 1
    assert len(motors["motors"]) == motors["rotor_count"]
    assert motors["airframe_family"], "the page would have no silhouette to draw"

    safety = safety_config.build(table, [])
    assert safety["sections"], "the Safety page would come up empty"

    tuning = tuning_config.build(table)
    assert tuning.get("groups") or tuning.get("sections")


def test_sensor_scenes_populate_the_sensor_half_of_the_safety_page() -> None:
    """The rangefinder/flow switches are the thing that screenshot shows."""
    bare = safety_config.build(params.build_table(), [])
    equipped = safety_config.build(
        params.build_table(rangefinder=True, optical_flow=True), [])
    assert len(equipped["sections"]) > len(bare["sections"])


def test_quad_geometry_is_a_quad() -> None:
    """Diagonal pairs spin against each other, at the arm length asked for."""
    motors = motor_config.build(params.build_table("quad_x"))["motors"]
    assert [m["number"] for m in motors] == [1, 2, 3, 4]
    assert {m["spin"] for m in motors} == {"CW", "CCW"}
    # PX4 numbers front-right, rear-left, front-left, rear-right; 1 and 2 are a
    # diagonal pair and therefore spin the same way.
    assert motors[0]["spin"] == motors[1]["spin"]
    assert motors[2]["spin"] == motors[3]["spin"] != motors[0]["spin"]
    arm = (motors[0]["x"] ** 2 + motors[0]["y"] ** 2) ** 0.5
    assert arm == pytest.approx(0.25, abs=0.005)


def test_larger_airframes_are_actually_larger() -> None:
    small = motor_config.build(params.build_table("quad_x"))["motors"][0]
    large = motor_config.build(params.build_table("quad_x_heavy"))["motors"][0]
    assert abs(large["x"]) > abs(small["x"])


def test_parameter_types_survive_the_table() -> None:
    """An int stays an int: the editor shows the type, so it is not cosmetic."""
    table = params.build_table()
    assert isinstance(table["CA_ROTOR_COUNT"], int)
    assert isinstance(table["CA_ROTOR0_PX"], float)


def test_the_full_table_is_firmware_sized_and_keeps_the_curated_values() -> None:
    base = params.build_table()
    full = params.build_full_table(base, total=1180)
    assert len(full) >= 1180
    for name, value in base.items():
        assert full[name] == value, "a generated filler overwrote a real value"


# --------------------------------------------------------------------------
# The flight is a flight
# --------------------------------------------------------------------------


def test_the_aircraft_climbs_then_flies() -> None:
    model = flight.FlightModel(flight.orbit(alt=60.0), takeoff_time=10.0)
    assert model.sample(0.0).alt_agl == pytest.approx(0.0, abs=0.01)
    assert model.sample(0.0).phase == "landed"
    assert model.sample(5.0).phase == "takeoff"
    assert model.sample(11.0).alt_agl == pytest.approx(60.0, abs=1.0)
    assert model.sample(30.0).phase == "cruise"


def test_the_track_goes_somewhere() -> None:
    model = flight.FlightModel(flight.survey(), takeoff_time=5.0)
    early = model.sample(10.0)
    later = model.sample(60.0)
    assert (early.lat, early.lon) != (later.lat, later.lon)
    assert later.ground_speed > 0


def test_the_battery_only_goes_down() -> None:
    model = flight.FlightModel(flight.orbit())
    readings = [model.battery(t)[2] for t in (0, 60, 300, 900)]
    assert readings == sorted(readings, reverse=True)
    assert readings[0] == 100


def test_bounds_hold_the_whole_path() -> None:
    scene = library.get("map")
    west, south, east, north = scene.bounds()
    path = flight.make_path(scene.path, home=scene.home, **scene.path_options)
    for point in path.points:
        assert west < point.lon < east
        assert south < point.lat < north


# --------------------------------------------------------------------------
# The simulated autopilot agrees with the bridge that talks to it
# --------------------------------------------------------------------------


@pytest.mark.parametrize("sensor", sorted(MavlinkBridge._CALIBRATION_MAP))
def test_the_vehicle_decodes_the_calibration_the_bridge_sends(sensor: str) -> None:
    """MAV_CMD_PREFLIGHT_CALIBRATION, encoded by Corvus and read back here.

    The parameter slots are PX4's, not ours, and they are the one part of the
    calibration screenshot that would fail silently: a wrong slot means the
    wizard starts and the aircraft never answers.
    """
    sent = MavlinkBridge._CALIBRATION_MAP[sensor]
    decoded = vehicle._selected_sensor(list(sent))
    # A quick accel calibration runs the accelerometer transcript.
    assert decoded == ("accel" if sensor == "accel_quick" else sensor)


def test_an_all_zero_calibration_command_is_the_cancel() -> None:
    assert vehicle._selected_sensor([0.0] * 7) is None


def test_mode_encoding_round_trips() -> None:
    sim = vehicle.SimVehicle(options=vehicle.VehicleOptions(mode="MISSION"))
    assert vehicle._mode_name(sim._custom_mode()) == "MISSION"
    sim.mode = "POSCTL"
    assert vehicle._mode_name(sim._custom_mode()) == "POSCTL"


def test_rc_pulses_are_in_range_and_follow_the_sticks() -> None:
    rc = vehicle.RcState(roll=1.0, throttle=0.0, jitter_us=0.0)
    pulses = rc.pulses(16)
    assert len(pulses) == 16
    assert all(1000 <= p <= 2000 for p in pulses)
    assert pulses[0] == 2000          # roll hard over
    assert pulses[2] == 1000          # throttle closed


# --------------------------------------------------------------------------
# The recipe clicks things that exist
# --------------------------------------------------------------------------


@pytest.mark.parametrize("scene_id", sorted(library.SCENES))
def test_a_recipe_is_complete(scene_id: str) -> None:
    scene = library.get(scene_id)
    steps = recipe.steps(scene, "http://127.0.0.1:8777/")
    actions = [s.action for s in steps]
    assert actions[0] == "resize"
    assert actions[-1] == "screenshot"
    assert "navigate" in actions
    assert scene.asset in steps[-1].detail


def test_the_bootstrap_carries_the_scene_s_appearance() -> None:
    js = recipe.bootstrap_js(library.get("dark"))
    assert "corvus.theme" in js and "green" in js
    assert "corvus.hud" in js
    assert "corvus.rc.transmitter.bindings.v2" in js


def test_navigation_uses_the_selectors_the_frontend_actually_ships() -> None:
    """The nav rail and the Setup tiles are buttons; these are their hooks."""
    src = (Path(__file__).resolve().parents[1] / "src" / "js")
    sidenav = (src / "sidenav.js").read_text(encoding="utf-8")
    setup = (src / "setup.js").read_text(encoding="utf-8")
    index = (Path(__file__).resolve().parents[1] / "src" / "index.html").read_text(
        encoding="utf-8")
    calibration = (src / "setup-calibration.js").read_text(encoding="utf-8")

    assert "dataset.nav" in sidenav, "the left nav no longer tags items with data-nav"
    assert "dataset.view" in setup, "the Setup tiles no longer tag themselves"
    assert 'id="panelHandle"' in index, "the workspace handle was renamed"
    assert 'id="rightPanel"' in index
    assert "card.dataset.type" in calibration, "the calibration cards lost data-type"

    js = recipe.navigate_js(library.get("motors"))
    assert '[data-nav="setup"]' in js
    assert '[data-view="motors"]' in js
    assert "#panelHandle" in js
    assert "#panelHandle" in js and "Corvus.panel.toggle" not in js, (
        "collapsing by calling toggle() is undone by the next window resize")


def test_the_calibration_recipe_opens_the_card_before_starting() -> None:
    js = recipe.calibration_js("accel")
    assert '[data-type="accel"]' in js
    assert js.index('[data-type="accel"]') < js.index("start")


def test_unknown_scene_fields_are_refused() -> None:
    with pytest.raises(KeyError):
        library.get("flight").with_overrides({"thheme": "blue"})


def test_overrides_apply() -> None:
    scene = library.get("flight").with_overrides({"theme": "blue", "settle": 5.0})
    assert scene.theme == "blue" and scene.settle == 5.0
    assert library.get("flight").theme == "light-orange", "the library was mutated"


# --------------------------------------------------------------------------
# The sortie the flying pictures share
# --------------------------------------------------------------------------


def test_the_hop_sags_right_of_the_straight_line() -> None:
    """The README's track: out of the start point, south of the chord, into the end."""
    path = flight.swoop()
    start, end = path.points[0], path.points[-1]
    assert (start.lat, start.lon) == pytest.approx(flight.CAMPUS_HOP_START)
    assert (end.lat, end.lon) == pytest.approx(flight.CAMPUS_HOP_END)
    mid = path.points[len(path.points) // 2]
    # Right of a north-easterly chord is south-east of it: the midpoint lies
    # below the straight line between the two ends.
    chord_lat = start.lat + (end.lat - start.lat) * (
        (mid.lon - start.lon) / (end.lon - start.lon))
    assert mid.lat < chord_lat


def test_every_flying_picture_flies_the_sortie() -> None:
    for scene in library.SCENES.values():
        if scene.page == "home":
            assert scene.path == library.SORTIE_PATH, scene.id
            assert scene.home == library.SORTIE_HOME, scene.id


def test_a_parked_aircraft_stands_still_on_the_ground() -> None:
    model = library.get("motors").build_model()
    fix = model.sample(30.0)
    assert fix.phase == "landed"
    assert fix.alt_agl == 0.0 and fix.ground_speed == 0.0
    assert model.battery(30.0)[1] < 2.0, "a disarmed aircraft draws no motor current"


# --------------------------------------------------------------------------
# The log Flight Review opens is a real ULog of that sortie
# --------------------------------------------------------------------------


@pytest.fixture(scope="module")
def sortie_log() -> bytes:
    return library.get("flight_review").build_flight_log()


def test_the_sortie_log_is_a_readable_ulog(sortie_log: bytes) -> None:
    from corvus import ulog

    log = ulog.read(sortie_log)
    assert not log.truncated
    assert log.info["sys_name"] == "PX4"
    assert log.params["SYS_AUTOSTART"] == library.get("flight_review").build_params()["SYS_AUTOSTART"]
    for topic in ("vehicle_attitude", "vehicle_local_position", "vehicle_gps_position",
                  "battery_status", "actuator_motors", "vehicle_status"):
        assert log.has(topic), topic


def test_the_sortie_log_reviews_clean(sortie_log: bytes) -> None:
    """A calm flight with a tuned aircraft: modes in order, nothing flagged."""
    from corvus import flight_review

    review = flight_review.review_bytes(sortie_log, "log_009.ulg")
    modes = [m["mode"] for m in review["modes"]]
    assert modes[:3] == ["Position", "Takeoff", "Mission"]
    assert "Return" in modes
    assert review["armed"], "the log has no armed span, so nothing flew"
    assert not [f for f in review["findings"] if f["level"] in ("warning", "critical")], (
        review["findings"])
    assert any(p["id"] == "track" for p in review["plots"])


def test_the_sortie_log_flies_where_the_map_pictures_fly(sortie_log: bytes) -> None:
    from corvus import ulog

    log = ulog.read(sortie_log)
    lats = [v / 1e7 for v in log.series("vehicle_gps_position", "lat")]
    lons = [v / 1e7 for v in log.series("vehicle_gps_position", "lon")]
    # Out to the end point and back to the start.
    far = max(range(len(lats)), key=lambda i: (lats[i] - lats[0]) ** 2 + (lons[i] - lons[0]) ** 2)
    assert (lats[far], lons[far]) == pytest.approx(flight.CAMPUS_HOP_END, abs=2e-5)
    assert (lats[-1], lons[-1]) == pytest.approx(flight.CAMPUS_HOP_START, abs=2e-5)


def test_the_newest_vehicle_log_carries_the_file() -> None:
    logs = library.get("analysis").build_logs()
    assert logs[-1].data and logs[-1].size == len(logs[-1].data)
    assert all(entry.data is None for entry in logs[:-1])


def test_the_vehicle_streams_the_whole_requested_span() -> None:
    """PX4 answers a LOG_REQUEST_DATA window with packets back to back."""
    blob = bytes(range(256)) * 4
    sim = vehicle.SimVehicle(options=vehicle.VehicleOptions(
        logs=(vehicle.LogEntry(id=1, utc=0, size=len(blob), data=blob),)))
    sent: list[tuple[int, bytes]] = []

    class _Mav:
        def log_data_send(self, log_id, ofs, count, data):
            sent.append((ofs, bytes(data[:count])))

    class _Conn:
        mav = _Mav()

    class _Msg:
        id, ofs, count = 1, 90, 500

    sim._conn = _Conn()
    sim._answer_log_data(_Msg())
    assert b"".join(chunk for _ofs, chunk in sent) == blob[90:590]
    assert [ofs for ofs, _chunk in sent][:2] == [90, 180]


# --------------------------------------------------------------------------
# The simulated companion computers answer real SSH
# --------------------------------------------------------------------------


def test_the_companion_computer_answers_ssh() -> None:
    paramiko = pytest.importorskip("paramiko")
    from scene_kit import companion

    server = companion.CompanionServer(port=0)
    server.start()
    client = paramiko.SSHClient()
    client.set_missing_host_key_policy(paramiko.AutoAddPolicy())
    try:
        client.connect("127.0.0.1", port=server.port, username="jetson",
                       password=companion.PASSWORD, look_for_keys=False,
                       allow_agent=False, timeout=10)
        channel = client.invoke_shell(width=40, height=20)
        import time as _time

        seen = b""
        deadline = _time.monotonic() + 15
        while b"192.168.144.20" not in seen and _time.monotonic() < deadline:
            if channel.recv_ready():
                seen += channel.recv(4096)
            else:
                _time.sleep(0.05)
        text = seen.decode("utf-8", "replace")
        assert "jetson@companion" in text
        assert "192.168.144.20" in text, "the scripted commands did not run"
        # Nothing the side panel shows may wrap at forty columns.
        for line in companion.JETSON.banner.splitlines():
            assert len(line) <= 40, line
    finally:
        client.close()
        server.stop()


def test_a_wrong_password_is_refused() -> None:
    paramiko = pytest.importorskip("paramiko")
    from scene_kit import companion

    server = companion.CompanionServer(port=0)
    server.start()
    client = paramiko.SSHClient()
    client.set_missing_host_key_policy(paramiko.AutoAddPolicy())
    try:
        with pytest.raises(paramiko.AuthenticationException):
            client.connect("127.0.0.1", port=server.port, username="jetson",
                           password="wrong", look_for_keys=False, allow_agent=False,
                           timeout=10)
    finally:
        client.close()
        server.stop()


# --------------------------------------------------------------------------
# No company logo, ever
# --------------------------------------------------------------------------


def test_the_sandbox_config_carries_no_company_logo(tmp_path: Path) -> None:
    import json

    from scene_kit.runner import RunnerOptions, SceneRunner

    sandbox = tmp_path / "home"
    (sandbox / ".corvus" / "branding").mkdir(parents=True)
    (sandbox / ".corvus" / "branding" / "logo.png").write_bytes(b"\x89PNG")
    runner = SceneRunner(library.get("flight"), RunnerOptions(sandbox=sandbox))
    config = json.loads(runner._write_config().read_text())
    assert "branding" not in config
    assert not (sandbox / ".corvus" / "branding").exists(), "a left-over logo would be served"
    assert config["ui"]["mission_page"] is True


@pytest.mark.parametrize("scene_id", sorted(library.SCENES))
def test_every_recipe_takes_the_logo_out_before_the_shot(scene_id: str) -> None:
    steps = recipe.steps(library.get(scene_id), "http://127.0.0.1:8777/")
    assert steps[-1].action == "screenshot"
    assert ".tb-company-logo" in steps[-2].payload


def test_the_logo_selector_is_the_one_the_top_bar_renders() -> None:
    topbar = (Path(__file__).resolve().parents[1] / "src" / "js" / "topbar.js").read_text(
        encoding="utf-8")
    assert 'className = "tb-company-logo"' in topbar


# --------------------------------------------------------------------------
# The new pages are reached the way the frontend builds them
# --------------------------------------------------------------------------


def test_the_new_pages_use_the_hooks_the_frontend_ships() -> None:
    src = Path(__file__).resolve().parents[1] / "src" / "js"
    analysis = (src / "analysis.js").read_text(encoding="utf-8")
    mission = (src / "mission.js").read_text(encoding="utf-8")
    panel = (src / "panel.js").read_text(encoding="utf-8")
    setup = (src / "setup.js").read_text(encoding="utf-8")
    sidenav = (src / "sidenav.js").read_text(encoding="utf-8")

    assert 'reviewTile.dataset.view = "review"' in analysis
    assert '"review-view-opt"' in analysis and "review-track-map" in analysis
    assert "setPlan:" in mission and 'id: "fit"' in mission
    assert "btn.dataset.name = dev.name" in panel
    assert 'makeTile("battery"' in setup
    assert 'id: "mission"' in sidenav

    js = recipe.navigate_js(library.get("battery"))
    assert '[data-view="battery"]' in js
    js = recipe.navigate_js(library.get("flight_review"))
    assert '.logs-tile[data-view="review"]' in js
    assert "Corvus.mission.setPlan" in recipe.mission_js(library.get("mission"))


def test_the_mission_plan_is_valid_for_the_backend() -> None:
    from corvus import mission as mission_mod

    checked, error = mission_mod.validate_plan(library.get("mission").mission())
    assert checked is not None, error
    assert [item["type"] for item in checked["items"]][0] == "takeoff"


def test_the_pictures_show_no_status_dots(tmp_path: Path) -> None:
    """The top bar's caption dots are off in every picture, as they are by default."""
    import json

    from scene_kit.runner import RunnerOptions, SceneRunner

    for scene in library.SCENES.values():
        assert scene.topbar_dots is False, scene.id
        assert '"corvus.topbarDots": "0"' in recipe.bootstrap_js(scene), scene.id
    # A sandbox of the test's own: the default one belongs to a scene that may
    # be running while the suite does.
    runner = SceneRunner(library.get("flight"), RunnerOptions(sandbox=tmp_path / "home"))
    config = json.loads(runner._write_config().read_text())
    assert config["ui"]["topbar_status_dots"] is False


def test_the_tuning_flight_turns_smoothly() -> None:
    """The rates the tuning charts draw are a figure of eight, not corner spikes."""
    import math

    model = library.get("tuning").build_model()
    peaks = []
    t = 6.0
    while t < 6.0 + model.duration:
        a, b = model.sample(t), model.sample(t + 0.1)
        peaks.append(abs(math.degrees(vehicle._wrap_pi(b.yaw - a.yaw) * 10)))
        t += 0.05
    assert max(peaks) < 45.0, "a vertex of the path shows up as a yaw-rate spike"
    assert sorted(peaks)[len(peaks) // 2] > 2.0, "the aircraft is not turning at all"


def test_flight_review_has_two_pictures_one_on_its_plots() -> None:
    """The review's head, and its plots: two pictures, no more."""
    reviews = [s for s in library.SCENES.values() if s.view == "review"]
    assert len(reviews) == 2
    focused = [s for s in reviews if s.review_focus]
    assert len(focused) == 1
    steps = recipe.steps(focused[0], "http://127.0.0.1:8777/")
    review = next(s.payload for s in steps if "review-pick" in s.payload)
    assert json.dumps(focused[0].review_focus.lower()) in review
    assert "satellite" not in review, "the plot picture does not wait for the ground track"


def test_the_sortie_log_has_no_corner_spikes() -> None:
    """A smooth curve flown smoothly: no attitude step at the path's vertices."""
    import math

    from corvus import ulog

    log = ulog.read(library.get("flight_review").build_flight_log())
    rates = [abs(math.degrees(row[0])) for row in log.series("vehicle_angular_velocity", "xyz")]
    assert max(rates) < 20.0, "a roll-rate spike means the path's corners reached the log"
