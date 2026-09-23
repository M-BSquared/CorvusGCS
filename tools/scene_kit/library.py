"""The scenes: one per picture the README shows, and the knobs to make more.

A scene is the whole state a screenshot needs, in one place — which aircraft is
on the wire, what it is doing, which theme the interface wears, which page is
open and what has to be clicked to get there. Regenerating a README image is
then ``run <scene>`` plus a screenshot, instead of rebuilding a flight, a
parameter set and a UI state from memory every time.

Adding a picture is adding a :class:`Scene` here, or passing ``--set`` on the
command line for a one-off. Nothing in this file is special-cased by the
runner: the built-in scenes use exactly the fields a new one would.

The captions are the README's own words for each image. They are kept beside
the scene deliberately — when a picture is regenerated, the sentence under it
is the thing most likely to have gone stale, and it should be visible at the
moment the picture is remade.
"""
from __future__ import annotations

from dataclasses import dataclass, field, replace
from typing import Any

from . import flight as flight_mod
from . import params as params_mod
from .vehicle import LogEntry, RcState

# Every theme in src/css/themes.css, as the settings page offers them.
THEMES = ("light-orange", "light", "green", "blue", "pink", "orange")

# The README's pictures are wide: a 16:10 window is what the layout is designed
# around, and it is what makes the map, the instrument panel and the workspace
# all readable in one frame.
DEFAULT_VIEWPORT = (1600, 1000)


@dataclass
class Scene:
    """One reproducible picture."""

    id: str
    asset: str                      # where it belongs in the repo
    title: str
    caption: str = ""               # the README's sentence under the image

    # --- the interface --------------------------------------------------
    theme: str = "light-orange"
    scale: float = 1.0
    viewport: tuple[int, int] = DEFAULT_VIEWPORT
    page: str = "home"              # home | setup | analysis | logs | settings
    view: str | None = None         # setup sub-page: motors, safety, calibration, …
    workspace: str = "open"         # open | collapsed
    tab: str = "link"               # link | console | ssh | future
    plugin: str | None = None       # plugin to open on the future tab, by its name
    hud: dict[str, Any] = field(default_factory=lambda: {
        "pinned": False, "compact": False, "collapsed": False, "readouts": True,
    })
    map_provider: str = "esri"
    map_layer: str = "esri_world_imagery"
    topbar_dots: bool = True
    virtual_joystick: bool = False

    # --- the aircraft ---------------------------------------------------
    airframe: str = params_mod.DEFAULT_AIRFRAME
    rangefinder: bool = False
    optical_flow: bool = False
    # Does this aircraft have a transmitter bound at all (the RC_* half of the
    # parameter table), and what is it sending?
    transmitter: bool = True
    rc_state: RcState = field(default_factory=RcState)
    param_overrides: dict[str, Any] = field(default_factory=dict)
    full_param_table: bool = False  # pad to a real firmware's ~1200 parameters

    # --- the flight -----------------------------------------------------
    home: tuple[float, float] = flight_mod.NEUBIBERG
    path: str = "orbit"
    path_options: dict[str, Any] = field(default_factory=dict)
    takeoff_time: float = 10.0
    armed: bool = True
    mode: str = "MISSION"

    # --- staging --------------------------------------------------------
    calibration: str | None = None          # accel | compass | gyro | level | …
    calibration_pause_after: int | None = None
    logs: int = 0                           # how many logs the card holds
    chatter: bool = True                    # scripted STATUSTEXT traffic
    settle: float = 20.0                    # seconds to let the scene build
    notes: str = ""                         # what to look for before shooting

    # -- derived ----------------------------------------------------------

    def build_params(self) -> dict[str, Any]:
        table = params_mod.build_table(
            self.airframe,
            rangefinder=self.rangefinder,
            optical_flow=self.optical_flow,
            rc=self.transmitter,
            overrides=self.param_overrides,
        )
        return params_mod.build_full_table(table) if self.full_param_table else table

    def build_model(self) -> flight_mod.FlightModel:
        path = flight_mod.make_path(self.path, home=self.home, **self.path_options)
        return flight_mod.FlightModel(path, home=self.home,
                                      takeoff_time=self.takeoff_time)

    def bounds(self) -> tuple[float, float, float, float]:
        """(west, south, east, north) around everything the picture must hold.

        The flown track, the home point, and a margin. The map framing step in
        the recipe fits to this, which is what stops a survey from being
        photographed with two of its legs off the side of the window.
        """
        path = flight_mod.make_path(self.path, home=self.home, **self.path_options)
        lats = [p.lat for p in path.points] + [self.home[0]]
        lons = [p.lon for p in path.points] + [self.home[1]]
        pad_lat = max(0.0006, (max(lats) - min(lats)) * 0.18)
        pad_lon = max(0.0009, (max(lons) - min(lons)) * 0.18)
        return (min(lons) - pad_lon, min(lats) - pad_lat,
                max(lons) + pad_lon, max(lats) + pad_lat)

    def build_logs(self) -> tuple[LogEntry, ...]:
        """A plausible log card: a morning's flights, newest last.

        Sizes and timestamps are generated from the count rather than listed,
        so asking for twelve logs does not mean inventing twelve dates.
        """
        if self.logs <= 0:
            return ()
        import time as _time

        base = int(_time.time()) - self.logs * 3600
        out = []
        for i in range(self.logs):
            minutes = 4 + (i * 7) % 23
            out.append(LogEntry(
                id=i + 1,
                utc=base + i * 1800,
                # ~1.1 MB per minute of flight is what a PX4 log at the default
                # profile actually weighs.
                size=int(minutes * 1_150_000 + (i * 37_000)),
            ))
        return tuple(out)

    def with_overrides(self, overrides: dict[str, Any]) -> "Scene":
        """A copy of this scene with *overrides* applied — the ``--set`` path."""
        known = {f.name for f in self.__dataclass_fields__.values()}  # type: ignore[attr-defined]
        bad = set(overrides) - known
        if bad:
            raise KeyError(f"unknown scene field(s): {', '.join(sorted(bad))}")
        return replace(self, **overrides)


# Scripted console traffic: (seconds after start, severity, text). This is what
# a PX4 aircraft actually says on a normal sortie, in the order it says it —
# the console screenshot is a screenshot of this list arriving.
def _chatter(mode: str = "MISSION") -> tuple[tuple[float, int, str], ...]:
    from pymavlink.dialects.v20 import common as m

    return (
        (1.0, m.MAV_SEVERITY_INFO, "[logger] file: /fs/microsd/log/2026-09-16"),
        (2.0, m.MAV_SEVERITY_INFO, "Using GPS 1 (NMEA, RTK fixed)"),
        (3.5, m.MAV_SEVERITY_INFO, "EKF2 GPS checks passed"),
        (5.0, m.MAV_SEVERITY_INFO, "Preflight checks passed"),
        (7.0, m.MAV_SEVERITY_NOTICE, "Armed by internal command"),
        (9.0, m.MAV_SEVERITY_INFO, f"Takeoff detected, mode {mode}"),
        (16.0, m.MAV_SEVERITY_INFO, "Takeoff altitude reached"),
        (22.0, m.MAV_SEVERITY_INFO, "Executing mission item 1"),
        (34.0, m.MAV_SEVERITY_INFO, "Executing mission item 2"),
        (41.0, m.MAV_SEVERITY_WARNING, "RTK base link degraded, float fix"),
        (48.0, m.MAV_SEVERITY_INFO, "RTK fixed"),
        (57.0, m.MAV_SEVERITY_INFO, "Executing mission item 3"),
        (70.0, m.MAV_SEVERITY_INFO, "Datalink regained"),
    )


CHATTER = _chatter


# ---------------------------------------------------------------------------
# The library
# ---------------------------------------------------------------------------

SCENES: dict[str, Scene] = {}


def _register(scene: Scene) -> Scene:
    SCENES[scene.id] = scene
    return scene


_register(Scene(
    id="flight",
    asset="assets/screenshot_flight.jpg",
    title="In flight — map, instrument panel and workspace",
    caption="Live map, the floating instrument panel, and the engineering "
            "workspace on the right.",
    page="home", workspace="open", tab="link",
    path="survey", path_options={"width": 320.0, "height": 210.0, "alt": 80.0},
    mode="MISSION", takeoff_time=8.0,
    settle=70.0,
    notes="Shoot once the survey has laid down three or four legs of track and "
          "the aircraft is mid-leg — a turn mid-frame hides the pattern.",
))

_register(Scene(
    id="map",
    asset="assets/screenshot_map.jpg",
    title="The map is the interface",
    caption="Collapse the side panel and the whole window becomes the "
            "operational picture — vehicle, heading, home point and the flown "
            "track, with the instrument panel wherever you put it.",
    page="home", workspace="collapsed",
    hud={"pinned": False, "compact": False, "collapsed": False, "readouts": True,
         "x": 40, "y": 120},
    path="orbit", path_options={"radius": 140.0, "alt": 65.0},
    mode="LOITER", takeoff_time=8.0,
    settle=80.0,
    notes="A full orbit is about 95 s at 9 m/s; wait for the circle to close "
          "before shooting, and move the instrument panel if it covers the track.",
))

_register(Scene(
    id="motors",
    asset="assets/screenshot_motors.png",
    title="Setup — the airframe drawn to scale",
    caption="Every motor at its real distance from the centre of gravity, with "
            "its number, its output pin and a spin-direction arrow.",
    page="setup", view="motors", workspace="collapsed",
    airframe="quad_x", armed=False, mode="POSCTL", path="hover",
    takeoff_time=0.0, settle=14.0,
    notes="Disarmed on purpose: the page refuses writes while armed, and the "
          "screenshot should show the editable state. Try airframe=hexa_x for "
          "the six-motor version.",
))

_register(Scene(
    id="safety",
    asset="assets/screenshot_safety.png",
    title="Setup — limits, failsafes and sensors",
    caption="Maximum distance and height, the return-to-launch profile, and an "
            "action for every loss PX4 can detect. A distance sensor or "
            "optical-flow camera comes up with one switch.",
    page="setup", view="safety", workspace="collapsed",
    rangefinder=True, optical_flow=True,
    armed=False, mode="POSCTL", path="hover", takeoff_time=0.0, settle=14.0,
    notes="Both sensors are on, so the driver-and-estimator pairing the caption "
          "describes is visible. Set rangefinder=False for the bare aircraft.",
))

_register(Scene(
    id="calibration",
    asset="assets/screenshot_calibration.png",
    title="Guided accelerometer calibration",
    caption="PX4 names the six accelerometer positions in its own order and in "
            "its own vocabulary — so each one is drawn instead of named.",
    page="setup", view="calibration", workspace="collapsed",
    armed=False, mode="POSCTL", path="hover", takeoff_time=0.0,
    calibration="accel", calibration_pause_after=3,
    settle=30.0,
    notes="The transcript stops with three sides ticked off and the fourth "
          "being asked for, and holds there until the scene is stopped. Change "
          "calibration_pause_after to catch a different step; calibration="
          "compass for the rotating one.",
))

_register(Scene(
    id="analysis",
    asset="assets/screenshot_analysis.png",
    title="Analysis — vehicle logs and Flight Review",
    caption="Download the vehicle's logs, record the live stream, and open "
            "either in the built-in Flight Review.",
    page="analysis", workspace="collapsed",
    logs=9, armed=False, mode="POSCTL", path="hover", takeoff_time=0.0,
    settle=25.0,
    notes="The vehicle answers the log protocol with nine logs, so the card "
          "lists them with real dates and sizes. A download runs and completes, "
          "but the bytes are filler — open a real .ulg in Flight Review if the "
          "picture needs a plot.",
))

_register(Scene(
    id="console",
    asset="assets/screenshot_console.jpg",
    title="The side workspace — MAVLink console",
    caption="A MAVLink console, an SSH terminal for the companion computer, "
            "and a plugin slot — beside the map rather than instead of it.",
    page="home", workspace="open", tab="console",
    path="out_and_back", mode="MISSION", takeoff_time=8.0,
    settle=75.0,
    notes="The console fills from the aircraft's own status text (see CHATTER) "
          "plus every command acknowledgement. Type a command in the console "
          "input before shooting if the picture should show the prompt in use.",
))

_register(Scene(
    id="plugins",
    asset="docs/assets/images/plugins.jpg",
    title="A plugin beside the map",
    caption="The Vibration Monitor plugin, live beside the map, on a short hop "
            "across the Neubiberg campus.",
    theme="light",
    page="home", workspace="open", tab="future", plugin="Vibration Monitor",
    home=flight_mod.CAMPUS_HOP_START,
    path="swoop", mode="MISSION", takeoff_time=8.0,
    settle=45.0,
    notes="The aircraft flies the curved hop once and holds at its end; the "
          "vibration chart needs about twenty seconds of data to read as a trace.",
))

_register(Scene(
    id="dark",
    asset="assets/screenshot_dark.jpg",
    title="A dark theme, in flight",
    caption="Six themes, two light and four dark. Switching is instant — no "
            "reload, no flash — and the map, the plots and the instruments all "
            "follow.",
    theme="green",
    page="home", workspace="open", tab="link",
    path="figure_eight", path_options={"size": 170.0, "alt": 55.0},
    mode="MISSION", takeoff_time=8.0, settle=70.0,
    notes="theme=green here; blue, pink and orange are the other dark ones. "
          "Shooting the same frame in two themes is one run each with --set theme=…",
))

_register(Scene(
    id="parameters",
    asset="assets/screenshot_parameters.png",
    title="The parameter editor",
    caption="The parameter editor in another theme.",
    theme="pink",
    page="setup", view="parameters", workspace="collapsed",
    full_param_table=True,
    armed=False, mode="POSCTL", path="hover", takeoff_time=0.0,
    settle=30.0,
    notes="The vehicle answers with a full-sized table (~1200 parameters), so "
          "the download has something to download and the list has weight. "
          "Press Download on the page, then shoot once it completes.",
))

_register(Scene(
    id="rc",
    asset="assets/screenshot_rc.png",
    title="Radio Control — the transmitter, drawn",
    caption="The handset as a drawing: sticks, six switches and two knobs, "
            "each one live against the channel bound to it.",
    page="setup", view="control", workspace="collapsed",
    armed=False, mode="POSCTL", path="hover", takeoff_time=0.0,
    # The arm switch (channel 7) is down, because this aircraft is disarmed:
    # a drawn transmitter showing arm-up beside a DISARMED top bar is the kind
    # of contradiction a reader who flies notices immediately.
    rc_state=RcState(roll=0.35, pitch=-0.2, throttle=0.62, yaw=-0.1,
                     switches={5: (2, 3), 6: (0, 2), 7: (0, 2), 8: (0, 2),
                               9: (1, 3), 10: (1, 2)},
                     knobs={11: 0.7, 12: 0.3}, rssi=94),
    settle=18.0,
    notes="The sticks sit off-centre on purpose — a drawn transmitter with "
          "every control centred looks like a diagram, not a reading. The "
          "switch-to-control binding lives in the browser, so the recipe seeds "
          "it in localStorage before the page opens.",
))

_register(Scene(
    id="tuning",
    asset="assets/screenshot_tuning.png",
    title="PID tuning, with the response drawn beside the setpoint",
    caption="Rate, attitude, velocity and position gains by hand, plus the "
            "in-flight autotune.",
    page="setup", view="tuning", workspace="collapsed",
    path="figure_eight", path_options={"size": 90.0, "alt": 40.0},
    mode="POSCTL", takeoff_time=6.0, settle=45.0,
    notes="Flying a tight figure of eight so the rate plots have something to "
          "draw. The aircraft stays armed here — the plots are the picture.",
))


def get(scene_id: str) -> Scene:
    try:
        return SCENES[scene_id]
    except KeyError:
        raise KeyError(
            f"unknown scene {scene_id!r}; known: {', '.join(sorted(SCENES))}") from None
