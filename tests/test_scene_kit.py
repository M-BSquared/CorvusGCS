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
