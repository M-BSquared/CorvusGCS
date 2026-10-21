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

# The sortie the flying pictures show: the curved hop across the field on the
# Neubiberg campus, from the start point to the end point in flight.py. Every
# picture with a map on it flies this, the mission planner draws it, and the
# Flight Review picture reviews its log, so the README tells one flight.
SORTIE_HOME = flight_mod.CAMPUS_HOP_START
SORTIE_PATH = "swoop"

# The README's pictures are wide: a 16:10 window is what the layout is designed
# around, and it is what makes the map, the instrument panel and the workspace
# all readable in one frame.
DEFAULT_VIEWPORT = (1600, 1000)

# The operator's own 20 cm surface model of the Neubiberg field (three 1 km
# DOM tiles, ETRS89 / UTM 32N, imported under Settings > Elevation). It lives
# in the shared tile cache, so it is only on a machine that imported it.
NEUBIBERG_DOM = "dem_11e6db89d6"


@dataclass(frozen=True)
class FleetVehicle:
    """One more vehicle on its own link, beside the scene's main aircraft.

    Corvus flies one autopilot per link, so each of these is a second
    simulated PX4 on a UDP port of its own, opened as an extra link with the
    name and colour the picture shows. ``kind`` is ``drone`` (a quadrotor) or
    ``rover`` (a ground vehicle, which stays at zero height).
    """

    name: str
    color: str
    kind: str = "drone"
    home: tuple[float, float] = SORTIE_HOME
    path: str = "orbit"
    path_options: dict[str, Any] = field(default_factory=dict)
    takeoff_time: float = 8.0
    armed: bool = True
    mode: str = "MISSION"

    def build_model(self) -> flight_mod.FlightModel:
        path = flight_mod.make_path(self.path, home=self.home, **self.path_options)
        return flight_mod.FlightModel(path, home=self.home,
                                      takeoff_time=self.takeoff_time)


@dataclass
class Scene:
    """One reproducible picture."""

    id: str
    asset: str                      # where it belongs in the repo
    title: str
    # Its name on the project website, docs/assets/images/<web>.jpg with an
    # 800 px copy beside it (see web.py); "" for a picture the site does not show.
    web: str = ""
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
    # No x/y: the instrument panel sits where the app puts it, at the bottom
    # right of the map.
    hud: dict[str, Any] = field(default_factory=lambda: {
        "pinned": False, "compact": False, "collapsed": False, "readouts": True,
    })
    # The panel's lock, compact and collapse controls, which the app shows only
    # under the pointer, shown for the picture.
    hud_controls: bool = True
    map_provider: str = "esri"
    map_layer: str = "esri_world_imagery"
    topbar_dots: bool = False       # the caption status dots (Settings > Status dots)
    virtual_joystick: bool = False
    mission_page: bool = True       # MISSION in the left rail (Settings > Pages)
    # The map's 3D mode for the picture, as the rail's 3D button sets it:
    # "off", "simple" (a tilt) or "full" (terrain, buildings, the aircraft at
    # its altitude). The bearing is where the tilted camera looks, and the zoom
    # is added to the fitted one, since a tilt shows more ground than a plan.
    map_3d: str = "off"
    map_bearing: float = 0.0
    map_zoom: float = 0.0
    # The elevation model 3D reads: "auto", or the id of a model imported
    # under Settings (a surface model of the field, say). A model the tile
    # cache does not hold falls back to the automatic one.
    terrain_source: str = "auto"
    # The interface language by its code in src/lang ("de", "fr"), None for
    # English, and any other key of the config's "ui" object (hud_square,
    # plugin_tabs, ...), as Settings would save them.
    language: str | None = None
    ui: dict[str, Any] = field(default_factory=dict)

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
    # The parameter editor's All / Modified / Unsaved filter, pressed for the
    # picture. "modified" leaves only the real, set-up rows beside their
    # defaults; the padding (see params._filler) is all at its default.
    param_filter: str = "all"

    # --- the flight -----------------------------------------------------
    home: tuple[float, float] = SORTIE_HOME
    path: str = "orbit"
    path_options: dict[str, Any] = field(default_factory=dict)
    takeoff_time: float = 10.0
    armed: bool = True
    mode: str = "MISSION"

    # --- staging --------------------------------------------------------
    calibration: str | None = None          # accel | compass | gyro | level | …
    calibration_pause_after: int | None = None
    logs: int = 0                           # how many logs the card holds
    # The newest of those logs already downloaded, as the ULog of the sortie
    # the map pictures show (see flight_log). Flight Review opens this file.
    review_log: bool = False
    # The Flight Review plot brought to the top of the picture, by its title;
    # "" frames the review's head: summary, modes, findings and ground track.
    review_focus: str = ""
    # A simulated companion computer on SSH (see companion), and the terminals
    # the recipe opens on it: the SSH tab, plus a terminal window.
    ssh: bool = False
    # The mission planner is seeded with a plan over the sortie's points.
    mission_plan: bool = False
    # Each plugin's saved settings, by plugin id, written to its own
    # config.json in the sandbox before the backend starts: a launcher's
    # shelf, the entities on the map, a trajectory file already read.
    plugin_settings: dict[str, dict[str, Any]] = field(default_factory=dict)
    # Named steps (recipe.STAGES) run once the scene has built and before the
    # map is framed: pressing a plugin's buttons, typing into a shell. Each one
    # is the browser half of a picture that no field above can describe.
    stage: tuple[str, ...] = ()
    # The geofence area kept on this station, as (lon, lat) corners, written
    # to the sandbox's geofence.json and shown on the Home map. Empty: none.
    geofence: tuple[tuple[float, float], ...] = ()
    # More vehicles, each on a link of its own (see FleetVehicle), and the
    # name and colour the main aircraft carries beside them. Empty: one
    # vehicle, and the interface behaves exactly as it does for one.
    fleet: tuple[FleetVehicle, ...] = ()
    vehicle_name: str = ""
    vehicle_color: str = ""
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

        The flown track, the home point, the geofence if there is one, and a
        margin. The map framing step in
        the recipe fits to this, which is what stops a survey from being
        photographed with two of its legs off the side of the window.
        """
        path = flight_mod.make_path(self.path, home=self.home, **self.path_options)
        lats = [p.lat for p in path.points] + [self.home[0]] + [c[1] for c in self.geofence]
        lons = [p.lon for p in path.points] + [self.home[1]] + [c[0] for c in self.geofence]
        for other in self.fleet:
            points = flight_mod.make_path(other.path, home=other.home,
                                          **other.path_options).points
            lats += [p.lat for p in points] + [other.home[0]]
            lons += [p.lon for p in points] + [other.home[1]]
        pad_lat = max(0.0006, (max(lats) - min(lats)) * 0.18)
        pad_lon = max(0.0009, (max(lons) - min(lons)) * 0.18)
        return (min(lons) - pad_lon, min(lats) - pad_lat,
                max(lons) + pad_lon, max(lats) + pad_lat)

    def build_logs(self) -> tuple[LogEntry, ...]:
        """A plausible log card: a morning's flights, newest last.

        Sizes and timestamps are generated from the count rather than listed,
        so asking for twelve logs does not mean inventing twelve dates. The
        newest one is the real ULog of the sortie (see :meth:`build_flight_log`),
        so a download of it gives a file Flight Review can open.
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
        blob = self.build_flight_log()
        newest = out[-1]
        out[-1] = LogEntry(id=newest.id, utc=newest.utc, size=len(blob), data=blob)
        return tuple(out)

    def build_flight_log(self) -> bytes:
        """The ULog of the sortie, whatever this scene's own aircraft is doing.

        Always the campus hop the flying pictures show, so the Flight Review
        picture reviews the same flight the map pictures draw.
        """
        from . import flight_log

        # Finely divided: the log's attitude comes from the path's curvature.
        path = flight_mod.make_path(SORTIE_PATH, home=SORTIE_HOME, points=240)
        return flight_log.build(SORTIE_HOME, path, self.build_params(),
                                chatter=(), boot_offset_s=12.0)

    def mission(self) -> dict[str, Any]:
        """The plan the mission planner is seeded with, over the sortie's points.

        The start, a takeoff, the curved hop as three waypoints, a circle over
        its end point and a return: the flight the map pictures show, drawn
        beforehand instead of flown.
        """
        path = flight_mod.make_path(SORTIE_PATH, home=SORTIE_HOME)
        points = path.points
        picks = [points[len(points) * k // 4] for k in (1, 2, 3)]
        end = points[-1]
        items: list[dict[str, Any]] = [
            {"type": "takeoff", "lat": SORTIE_HOME[0], "lon": SORTIE_HOME[1],
             "alt": 40.0, "pitch": 0, "name": "Climb out"},
        ]
        for index, point in enumerate(picks, 1):
            items.append({"type": "waypoint", "lat": round(point.lat, 7),
                          "lon": round(point.lon, 7), "alt": 40.0, "hold": 0,
                          "accept_radius": 0, "name": f"Field {index}"})
        items.append({"type": "loiter_turns", "lat": round(end.lat, 7),
                      "lon": round(end.lon, 7), "alt": 45.0, "turns": 2,
                      "radius": 25, "direction": 1, "name": "Survey point"})
        items.append({"type": "rtl"})
        return {
            "version": 1,
            "name": "Campus hop",
            "home": {"lat": SORTIE_HOME[0], "lon": SORTIE_HOME[1]},
            "speed": 6.0,
            "items": items,
        }

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


# The flying pictures share one flight: the sortie above, lifted off at the
# start point, flown once and held at the end point. The aircraft takes about
# 45 s to get there, so these settle for 50 s and are shot with the whole
# curve drawn and the aircraft standing at its end.
_SORTIE = dict(home=SORTIE_HOME, path=SORTIE_PATH, mode="MISSION", takeoff_time=8.0)
_SORTIE_NOTE = ("The aircraft flies the curved hop across the field once and "
                "holds at its end; shoot once it has arrived, with the whole "
                "curve drawn from the start point.")

_register(Scene(
    id="flight",
    asset="assets/screenshot_flight.jpg",
    web="flight-light",
    title="In flight: map, instrument panel and workspace",
    caption="Live map, the floating instrument panel, and the engineering "
            "workspace on the right.",
    page="home", workspace="open", tab="link",
    **_SORTIE,
    settle=50.0,
    notes=_SORTIE_NOTE,
))

_register(Scene(
    id="map",
    asset="assets/screenshot_map.jpg",
    web="map",
    title="The map is the interface",
    caption="Collapse the side panel and the whole window becomes the "
            "operational picture: vehicle, heading, home point and the flown "
            "track, with the instrument panel wherever you put it.",
    page="home", workspace="collapsed",
    **_SORTIE,
    settle=50.0,
    notes=_SORTIE_NOTE,
))

_register(Scene(
    id="three_d",
    asset="docs/assets/images/map-3d-dom.jpg",
    web="map-3d-dom",
    title="The map in 3D",
    caption="The field from a 20 cm surface model: every tree, every roof and "
            "the aircraft drawn at the altitude it is flying.",
    theme="blue",
    page="home", workspace="collapsed",
    map_3d="simple", map_bearing=20.0, map_zoom=0.6,
    terrain_source=NEUBIBERG_DOM,
    **_SORTIE,
    settle=50.0,
    notes=_SORTIE_NOTE + " The ground is the imported DOM, buildings and trees "
          "included, so the simple 3D (terrain without the OpenStreetMap "
          "blocks) is the one that shows it best. Without the model the "
          "picture falls back to the free 30 m model and the field is flat.",
))

_register(Scene(
    id="mission",
    asset="assets/screenshot_mission.jpg",
    web="mission",
    title="The mission planner",
    caption="A flight drawn before it is flown: start point, waypoints, an "
            "orbit and the return, with the altitude profile underneath.",
    page="mission", workspace="collapsed",
    home=SORTIE_HOME, armed=False, mode="POSCTL", path="parked", takeoff_time=0.0,
    mission_plan=True,
    settle=12.0,
    notes="The plan is the sortie the map pictures fly, seeded into the page "
          "and framed with its own fit button. Nothing is uploaded.",
))

_register(Scene(
    id="setup",
    asset="assets/screenshot_setup.png",
    web="setup",
    title="Setup: every configuration page, one tile each",
    caption="Calibration, radio, tuning, motors, safety, battery, telemetry "
            "radio, RTK, Remote ID, parameters, firmware and video.",
    page="setup", workspace="collapsed",
    armed=False, mode="POSCTL", path="parked", takeoff_time=0.0, settle=10.0,
    notes="The vehicle info card at the top fills in once AUTOPILOT_VERSION "
          "has arrived, a few seconds after the link comes up.",
))

_register(Scene(
    id="motors",
    asset="assets/screenshot_motors.png",
    web="motors",
    title="Setup: the airframe drawn to scale",
    caption="Every motor at its real distance from the centre of gravity, with "
            "its number, its output pin and a spin-direction arrow.",
    page="setup", view="motors", workspace="collapsed",
    airframe="quad_x", armed=False, mode="POSCTL", path="parked",
    takeoff_time=0.0, settle=14.0,
    notes="Disarmed on purpose: the page refuses writes while armed, and the "
          "screenshot should show the editable state. Try airframe=hexa_x for "
          "the six-motor version.",
))

_register(Scene(
    id="battery",
    asset="assets/screenshot_battery.png",
    web="battery",
    title="Setup: battery and power",
    caption="Cells, capacity, the power module and which charge reading the "
            "top bar shows.",
    page="setup", view="battery", workspace="collapsed",
    armed=False, mode="POSCTL", path="parked", takeoff_time=0.0, settle=14.0,
    notes="A 6S 16 Ah pack on a power module, read back from BAT1_*.",
))

_register(Scene(
    id="safety",
    asset="assets/screenshot_safety.png",
    web="safety",
    title="Setup: limits, failsafes and sensors",
    caption="Maximum distance and height, the return-to-launch profile, and an "
            "action for every loss PX4 can detect. A distance sensor or "
            "optical-flow camera comes up with one switch.",
    page="setup", view="safety", workspace="collapsed",
    rangefinder=True, optical_flow=True,
    armed=False, mode="POSCTL", path="parked", takeoff_time=0.0, settle=14.0,
    notes="Both sensors are on, so the driver-and-estimator pairing the caption "
          "describes is visible. Set rangefinder=False for the bare aircraft.",
))

_register(Scene(
    id="calibration",
    asset="assets/screenshot_calibration.png",
    web="calibration",
    title="Guided accelerometer calibration",
    caption="PX4 names the six accelerometer positions in its own order and in "
            "its own vocabulary, so each one is drawn instead of named.",
    page="setup", view="calibration", workspace="collapsed",
    armed=False, mode="POSCTL", path="parked", takeoff_time=0.0,
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
    title="Analysis: vehicle logs and Flight Review",
    caption="Download the vehicle's logs, record the live stream, and open "
            "either in the built-in Flight Review.",
    page="analysis", workspace="collapsed",
    logs=9, review_log=True,
    armed=False, mode="POSCTL", path="parked", takeoff_time=0.0,
    settle=15.0,
    notes="The vehicle answers the log protocol with nine logs. The newest is "
          "a real ULog of the sortie, already in the download folder, so the "
          "Flight Review tile has a log ready to open.",
))

_register(Scene(
    id="flight_review",
    asset="assets/screenshot_flight_review.png",
    web="flight-review",
    title="Flight Review of the sortie",
    caption="A downloaded ULog reduced to the plots that decide whether a "
            "flight was healthy, with the flight modes behind every trace.",
    page="analysis", view="review", workspace="collapsed",
    logs=9, review_log=True,
    armed=False, mode="POSCTL", path="parked", takeoff_time=0.0,
    settle=12.0,
    notes="The log is the ULog of the same hop the map pictures fly, written "
          "by the simulation (see flight_log.py), not a flight somebody flew.",
))

_register(Scene(
    id="flight_review_charts",
    asset="assets/screenshot_flight_review_charts.png",
    web="flight-review-charts",
    title="Flight Review: estimate against setpoint",
    caption="Every plot carries the flight modes behind it. Here the pitch angle "
            "and its rate, estimate against setpoint, from the takeoff through "
            "the mission to the return.",
    page="analysis", view="review", workspace="collapsed",
    logs=9, review_log=True, review_focus="Pitch angle",
    armed=False, mode="POSCTL", path="parked", takeoff_time=0.0,
    settle=6.0,
    notes="The same log as flight_review, scrolled to the control plots. Set "
          "review_focus to another plot's title (Speed, Motor outputs, Battery) "
          "to frame that one instead.",
))

_register(Scene(
    id="console",
    asset="assets/screenshot_console.jpg",
    web="console",
    title="The side workspace: MAVLink console",
    caption="A MAVLink console, an SSH terminal for the companion computer, "
            "and a plugin slot, beside the map rather than instead of it.",
    page="home", workspace="open", tab="console",
    **_SORTIE,
    settle=50.0,
    notes="The console fills from the aircraft's own status text (see CHATTER) "
          "plus every command acknowledgement. Type a command in the console "
          "input before shooting if the picture should show the prompt in use.",
))

_register(Scene(
    id="ssh",
    asset="assets/screenshot_ssh.jpg",
    web="ssh",
    title="SSH terminals on the companion computers",
    caption="A real terminal on the companion computer in the side panel, and "
            "a second one in a window of its own over the map.",
    page="home", workspace="open", tab="ssh", ssh=True,
    **_SORTIE,
    settle=50.0,
    notes="Both computers are simulated by companion.py on 127.0.0.1; the "
          "sessions are real SSH, and each types a few commands on login.",
))

_register(Scene(
    id="plugins",
    asset="docs/assets/images/plugins.jpg",
    web="plugins",
    title="A plugin beside the map",
    caption="The Vibration Monitor plugin, live beside the map, on a short hop "
            "across the Neubiberg campus.",
    theme="light",
    page="home", workspace="open", tab="future", plugin="Vibration Monitor",
    **_SORTIE,
    settle=50.0,
    notes=_SORTIE_NOTE + " The vibration chart needs about twenty seconds of "
          "data to read as a trace.",
))

# --- the new pages --------------------------------------------------------

_register(Scene(
    id="live_messages",
    asset="assets/screenshot_live_messages.png",
    web="live-messages",
    title="Live Messages: every MAVLink message on the link",
    caption="Every message type the aircraft sends, with its rate, its age and "
            "how often it came, and the fields of the one you pick.",
    page="analysis", view="scope", workspace="collapsed",
    **_SORTIE,
    stage=("scope",),
    settle=20.0,
    notes="The aircraft is flying, so the attitude, position and status "
          "streams all run at their real rates. ATTITUDE is picked so its "
          "fields fill the right side.",
))

_register(Scene(
    id="parameter_help",
    asset="docs/assets/images/parameter-help.jpg",
    web="parameter-help",
    title="Every parameter explained",
    caption="The circled i beside a name says what the parameter does, its "
            "default, its range and its values.",
    theme="blue",
    page="setup", view="parameters", workspace="collapsed",
    full_param_table=True, param_filter="modified",
    armed=False, mode="POSCTL", path="parked", takeoff_time=0.0,
    stage=("param_hint",),
    settle=30.0,
    notes="The same editor as the parameters picture, in the blue theme, with "
          "the hint of the battery failsafe open: its default and its values.",
))

_register(Scene(
    id="german",
    asset="assets/screenshot_german.jpg",
    web="flight-german",
    title="The interface in German",
    caption="English, German and French, one file each. The map, "
            "the instruments and every page follow the language you pick.",
    theme="light",
    language="de",
    page="home", workspace="open", tab="link",
    **_SORTIE,
    settle=50.0,
    notes=_SORTIE_NOTE + " The language comes from the config, as Settings "
          "would save it.",
))


# --- one picture per plugin -----------------------------------------------
#
# Every plugin opens on the PLUGINS tab beside the sortie, with the settings an
# operator would have saved for it written into its own config.json first.

def _offset(lat: float, lon: float, east_m: float, north_m: float) -> tuple[float, float]:
    """A point *east_m* and *north_m* metres from (lat, lon)."""
    import math

    return (round(lat + north_m / 111_195.0, 7),
            round(lon + east_m / (111_195.0 * math.cos(math.radians(lat))), 7))


def _sortie_point(fraction: float) -> tuple[float, float]:
    path = flight_mod.make_path(SORTIE_PATH, home=SORTIE_HOME)
    point = path.points[min(len(path.points) - 1, int(len(path.points) * fraction))]
    return point.lat, point.lon


def _entities() -> list[dict[str, Any]]:
    """What Map Entities has placed: a gate across the hop, trees beside it,
    a radar's reach south of the start and the people at the start."""
    gate_lat, gate_lon = _sortie_point(0.5)
    a = _offset(gate_lat, gate_lon, -5.0, 11.0)
    b = _offset(gate_lat, gate_lon, 5.0, -11.0)
    home_lat, home_lon = SORTIE_HOME

    def point(kind: str, name: str, at: tuple[float, float], **extra: Any) -> dict[str, Any]:
        return {"id": name.lower().replace(" ", "-"), "kind": kind, "name": name,
                "lat": at[0], "lng": at[1], "note": "", **extra}

    end_lat, end_lon = _sortie_point(1.0)
    return [
        {"id": "field-gate", "kind": "gate", "name": "Field gate", "color": "#FFD60A",
         "lat": a[0], "lng": a[1], "lat2": b[0], "lng2": b[1], "base": 30, "height": 20,
         "note": "Fly through at 40 m"},
        point("tree", "Oak", _offset(gate_lat, gate_lon, -38.0, 24.0), radius=6, height=18),
        point("tree", "Lime", _offset(gate_lat, gate_lon, 32.0, 30.0), radius=5, height=14),
        point("radar", "Radar", _offset(home_lat, home_lon, 70.0, -55.0),
              radius=70, alert=True, angle=110, bearing=15),
        point("pad", "Pad", (home_lat, home_lon)),
        point("person", "Pilot", _offset(home_lat, home_lon, -8.0, -14.0)),
        point("vehicle", "Ground station", _offset(home_lat, home_lon, -35.0, -5.0)),
        point("camera", "Survey camera", _offset(end_lat, end_lon, 18.0, 14.0),
              radius=35, alert=False, angle=360, bearing=0),
    ]


def _reference_route() -> str:
    """The route the Trajectory Viewer reads: the hop as planned, a few metres
    off the curve the aircraft flies, as WGS84 longitude, latitude, altitude."""
    import math

    path = flight_mod.make_path(SORTIE_PATH, home=SORTIE_HOME, points=60)
    lines = []
    for i, p in enumerate(path.points):
        swing = 7.0 * math.sin(i / (len(path.points) - 1) * math.pi * 2.0)
        lat, lon = _offset(p.lat, p.lon, -swing * 0.4, swing)
        lines.append(f"{lon} {lat} 40")
    return "\n".join(lines)


_PLUGIN_NOTE = (_SORTIE_NOTE + " The plugin's settings are written to its "
                "config.json in the sandbox before the backend starts.")

_register(Scene(
    id="plugin_ssh_launcher",
    asset="docs/assets/images/plugin-ssh-launcher.jpg",
    web="plugin-ssh-launcher",
    title="SSH Launcher: programs on the companion computer, one press each",
    caption="A shelf of buttons that start the mission script, the recorder or "
            "the gimbal on the companion computer, each in a terminal of its own.",
    page="home", workspace="open", tab="future", plugin="SSH Launcher", ssh=True,
    plugin_settings={"ssh-launcher": {"buttons": [
        {"id": "mission", "label": "Mission script", "connection": "Mission computer",
         "directory": "~/mission", "command": "./start_mission.sh", "mode": "terminal"},
        {"id": "recorder", "label": "Recorder", "connection": "Mission computer",
         "directory": "~/payload", "command": "python3 record.py --out /data/flights",
         "mode": "terminal", "restart": True},
        {"id": "gimbal", "label": "Gimbal", "connection": "Mission computer",
         "directory": "~/ws", "command": "ros2 launch payload gimbal.launch.py",
         "mode": "terminal"},
        {"id": "router", "label": "Restart MAVLink router", "connection": "Mission computer",
         "directory": "~", "command": "systemctl restart mavlink-router", "mode": "background"},
    ]}},
    stage=("launcher", "launcher_window", "place_window"),
    **_SORTIE,
    settle=50.0,
    notes=_PLUGIN_NOTE + " The first two buttons are pressed and the first "
          "one's terminal is opened over the map.",
))

_register(Scene(
    id="plugin_schwalby",
    asset="docs/assets/images/plugin-schwalby.jpg",
    web="plugin-schwalby",
    title="Schwalby: programs here or over SSH",
    caption="Buttons that start a program on this laptop or on the companion "
            "computer, with the companion's status above them.",
    theme="green",
    page="home", workspace="open", tab="future", plugin="Schwalby", ssh=True,
    plugin_settings={"schwalby": {
        "companion": {"name": "Companion", "host": "127.0.0.1", "interval_s": 2,
                      "timeout_s": 6, "connection": "Mission computer"},
        "buttons": [
            {"id": "mission", "label": "Mission script", "target": "ssh",
             "connection": "Mission computer", "directory": "~/mission",
             "command": "./start_mission.sh", "mode": "terminal"},
            {"id": "sync", "label": "Copy logs to the NAS", "target": "local",
             "directory": "~/flights", "command": "./sync_logs.sh", "mode": "background"},
            {"id": "recorder", "label": "Recorder", "target": "ssh",
             "connection": "Mission computer", "directory": "~/payload",
             "command": "python3 record.py --out /data/flights", "mode": "terminal",
             "restart": True},
            {"id": "disk", "label": "Free space on /data", "target": "ssh",
             "connection": "Mission computer", "directory": "~",
             "command": "df -h /data", "mode": "terminal"},
        ]}},
    stage=("schwalby",),
    **_SORTIE,
    settle=50.0,
    notes=_PLUGIN_NOTE + " Only the buttons that run over SSH are pressed: a "
          "local one would open a shell on the machine taking the picture.",
))

_register(Scene(
    id="plugin_map_entities",
    asset="assets/screenshot_map_entities.jpg",
    web="plugin-map-entities",
    title="Map Entities: a gate, trees and a radar on the map",
    caption="Gates, trees and devices with a range, placed on the map. The "
            "aircraft's pass through the gate is checked as it flies.",
    page="home", workspace="open", tab="future", plugin="Map Entities",
    plugin_settings={"map-entities": {"entities": _entities()}},
    stage=("entity",),
    **_SORTIE,
    settle=50.0,
    notes=_PLUGIN_NOTE + " The gate stands across the hop with its opening "
          "from 30 to 50 m, so the aircraft at 40 m flies through it.",
))

_register(Scene(
    id="plugin_nuttx_console",
    asset="docs/assets/images/plugin-nuttx-console.jpg",
    web="plugin-nuttx-console",
    title="NuttX Console: the PX4 shell over MAVLink",
    caption="NSH on the flight controller, over the telemetry link: ver, free, "
            "listener and every other builtin, with no cable.",
    theme="orange",
    page="home", workspace="open", tab="future", plugin="NuttX Console",
    stage=("nuttx",),
    **_SORTIE,
    settle=50.0,
    notes=_PLUGIN_NOTE + " The aircraft answers SERIAL_CONTROL with the shell "
          "in nsh.py.",
))

_register(Scene(
    id="plugin_trajectory_viewer",
    asset="docs/assets/images/plugin-trajectory-viewer.jpg",
    web="plugin-trajectory-viewer",
    title="Trajectory Viewer: a reference route under the flown track",
    caption="A route from a file, drawn under the flown track, so you see at a "
            "glance where the aircraft leaves it.",
    page="home", workspace="open", tab="future", plugin="Trajectory Viewer",
    plugin_settings={"trajectory-viewer": {
        "file": "campus_hop_planned.txt", "points": _reference_route(),
        "frame": "wgs84", "color": "magenta", "drawn": True}},
    **_SORTIE,
    settle=50.0,
    notes=_PLUGIN_NOTE + " The route is the hop as planned, swinging a few "
          "metres either side of the curve the aircraft flies.",
))

# --- the geofence ---------------------------------------------------------

def _sortie_fence() -> tuple[tuple[float, float], ...]:
    """The area the sortie is flown in: the field the hop crosses, traced
    along the inside of the track around it, as (lon, lat) corners.

    Eight corners, east and north of the start point in metres, the way an
    operator would click them along the field's edge on the large map.
    """
    home_lat, home_lon = SORTIE_HOME
    corners = ((-47.0, 142.0), (72.0, 123.0), (172.0, 157.0), (200.0, 145.0),
               (223.0, 92.0), (221.0, 26.0), (-21.0, -46.0), (-54.0, 57.0))
    out = []
    for east, north in corners:
        lat, lon = _offset(home_lat, home_lon, east, north)
        out.append((lon, lat))
    return tuple(out)


_FENCE_NOTE = ("The area is eight corners along the field the hop crosses, "
               "kept in the sandbox's geofence.json and uploaded to the "
               "simulated aircraft over the mission protocol (FENCE).")

_register(Scene(
    id="geofence",
    asset="docs/assets/images/geofence.jpg",
    web="geofence",
    title="The geofence: the area the aircraft may fly in",
    caption="The area on a map of its own, uploaded to the aircraft as an "
            "inclusion fence, with the action when it is left.",
    page="setup", view="safety", workspace="collapsed",
    armed=False, mode="POSCTL", path="parked", takeoff_time=0.0,
    geofence=_sortie_fence(),
    stage=("geofence_upload", "geofence_card"),
    settle=14.0,
    notes=_FENCE_NOTE + " The card is scrolled into view after the upload, "
          "so the line under its map says the vehicle has this area.",
))

_register(Scene(
    id="geofence_edit",
    asset="docs/assets/images/geofence-edit.jpg",
    web="geofence-edit",
    title="Drawing the geofence on the large map",
    caption="Drag a corner, add one in the middle of an edge, or draw the area "
            "again, on a map that fills the window.",
    theme="blue",
    page="setup", view="safety", workspace="collapsed",
    armed=False, mode="POSCTL", path="parked", takeoff_time=0.0,
    geofence=_sortie_fence(),
    stage=("geofence_upload", "geofence_large"),
    settle=14.0,
    notes=_FENCE_NOTE + " The large editor is opened with the card's own "
          "button and framed on the area.",
))

_register(Scene(
    id="geofence_3d",
    asset="docs/assets/images/geofence-dom.jpg",
    web="geofence-dom",
    title="The geofence in 3D, over a 20 cm surface model",
    caption="The field in 3D from the survey office's 20 cm surface model, "
            "trees and roofs included, with the geofence around it and "
            "the aircraft at its altitude.",
    page="home", workspace="collapsed",
    map_3d="simple", map_bearing=25.0, map_zoom=0.5,
    terrain_source=NEUBIBERG_DOM,
    geofence=_sortie_fence(),
    stage=("geofence_upload",),
    **_SORTIE,
    settle=50.0,
    notes=_SORTIE_NOTE + " " + _FENCE_NOTE + " The ground is the imported DOM "
          "(" + NEUBIBERG_DOM + "), read from the tile cache; without it the "
          "picture falls back to the free 30 m model and the trees go flat.",
))

_register(Scene(
    id="geofence_map",
    asset="docs/assets/images/geofence-map.jpg",
    web="geofence-map",
    title="The geofence on the Home map, in flight",
    caption="The area drawn as a dashed outline under the flown track, so the "
            "edge is in sight while you fly.",
    page="home", workspace="collapsed",
    geofence=_sortie_fence(),
    stage=("geofence_upload",),
    **_SORTIE,
    settle=50.0,
    notes=_SORTIE_NOTE + " " + _FENCE_NOTE,
))


# --- several vehicles ----------------------------------------------------

def _fleet() -> tuple[FleetVehicle, ...]:
    """A second quadrotor and a rover on the field the sortie crosses.

    Each is its own link, as it is in the field: one radio per vehicle. The
    quadrotor flies one gentle curve along the north of the field, short of
    the buildings behind it, and the rover drives snaking lines across the
    meadow at walking pace, at zero height.
    """
    home_lat, home_lon = SORTIE_HOME
    scout_start = _offset(home_lat, home_lon, -10.0, 105.0)
    scout_end = _offset(home_lat, home_lon, 185.0, 125.0)
    rover_start = _offset(home_lat, home_lon, 25.0, 62.0)
    rover_end = _offset(home_lat, home_lon, 130.0, 76.0)
    return (
        FleetVehicle("Zippy", "#2F7DF6", kind="drone", home=scout_start, path="swoop",
                     path_options={"end": list(scout_end), "bulge": -15.0,
                                   "alt": 45.0, "speed": 7.0}),
        FleetVehicle("Rollo", "#1FA971", kind="rover", home=rover_start,
                     path="slalom", takeoff_time=0.2,
                     path_options={"end": list(rover_end), "amplitude": 6.0,
                                   "wavelength": 26.0, "speed": 2.5}),
    )


_FLEET_NOTE = ("Three vehicles on three links: the main aircraft on the main "
               "link, a second quadrotor and a rover on extra links, each a "
               "simulated PX4 on a UDP port of its own. Names and colours are "
               "saved in the sandbox's config the way the Vehicles dialog "
               "saves them.")

_register(Scene(
    id="fleet",
    asset="docs/assets/images/fleet.jpg",
    web="fleet",
    title="Several vehicles on one map",
    caption="Two drones and a rover on one map, each in its own colour with "
            "its name under the marker and a track of its own.",
    page="home", workspace="collapsed",
    fleet=_fleet(), vehicle_name="Bumble",
    **_SORTIE,
    settle=50.0,
    notes=_SORTIE_NOTE + " " + _FLEET_NOTE,
))

_register(Scene(
    id="fleet_list",
    asset="docs/assets/images/fleet-vehicles.jpg",
    web="fleet-vehicles",
    title="The Vehicles dialog: drones and a rover in one list",
    caption="Every vehicle the station hears, with its name, colour, mode and "
            "battery, and the button that takes control of it.",
    theme="blue",
    page="home", workspace="collapsed",
    fleet=_fleet(), vehicle_name="Bumble",
    stage=("vehicles_dialog",),
    **_SORTIE,
    settle=50.0,
    notes=_SORTIE_NOTE + " " + _FLEET_NOTE + " The dialog is opened from the "
          "top bar's Vehicle block once all three are listed.",
))


_register(Scene(
    id="dark",
    asset="assets/screenshot_dark.jpg",
    web="flight-dark",
    title="A dark theme, in flight",
    caption="Six themes, two light and four dark. Switching is instant, with "
            "no reload and no flash, and the map, the plots and the "
            "instruments all follow.",
    theme="green",
    page="home", workspace="open", tab="link",
    **_SORTIE,
    settle=50.0,
    notes="theme=green here; blue, pink and orange are the other dark ones. "
          "Shooting the same frame in two themes is one run each with --set theme=…",
))

_register(Scene(
    id="parameters",
    asset="assets/screenshot_parameters.png",
    web="parameters",
    title="The parameter editor",
    caption="The parameter editor in another theme.",
    theme="pink",
    page="setup", view="parameters", workspace="collapsed",
    full_param_table=True, param_filter="modified",
    armed=False, mode="POSCTL", path="parked", takeoff_time=0.0,
    settle=30.0,
    notes="The vehicle answers with a full-sized table (~1200 parameters), so "
          "the download has something to download and the list has weight. "
          "The Modified filter is pressed for the shot: the rows a set-up "
          "aircraft changed, each beside its PX4 default.",
))

_register(Scene(
    id="rc",
    asset="assets/screenshot_rc.png",
    web="rc",
    title="Radio Control: the transmitter, drawn",
    caption="The handset as a drawing: sticks, six switches and two knobs, "
            "each one live against the channel bound to it.",
    page="setup", view="control", workspace="collapsed",
    armed=False, mode="POSCTL", path="parked", takeoff_time=0.0,
    # The arm switch (channel 7) is down, because this aircraft is disarmed:
    # a drawn transmitter showing arm-up beside a DISARMED top bar is the kind
    # of contradiction a reader who flies notices immediately.
    rc_state=RcState(roll=0.35, pitch=-0.2, throttle=0.62, yaw=-0.1,
                     switches={5: (2, 3), 6: (0, 2), 7: (0, 2), 8: (0, 2),
                               9: (1, 3), 10: (1, 2)},
                     knobs={11: 0.7, 12: 0.3}, rssi=94),
    settle=18.0,
    notes="The sticks sit off-centre on purpose: a drawn transmitter with "
          "every control centred looks like a diagram, not a reading. The "
          "switch-to-control binding lives in the browser, so the recipe seeds "
          "it in localStorage before the page opens.",
))

_register(Scene(
    id="tuning",
    asset="assets/screenshot_tuning.png",
    web="tuning",
    title="PID tuning, with the response drawn beside the setpoint",
    caption="Rate, attitude, velocity and position gains by hand, plus the "
            "in-flight autotune.",
    page="setup", view="tuning", workspace="collapsed",
    path="figure_eight", path_options={"size": 90.0, "alt": 40.0},
    mode="POSCTL", takeoff_time=6.0, settle=45.0,
    notes="Flying a tight figure of eight so the rate plots have something to "
          "draw. The aircraft stays armed here: the plots are the picture.",
))


def get(scene_id: str) -> Scene:
    try:
        return SCENES[scene_id]
    except KeyError:
        raise KeyError(
            f"unknown scene {scene_id!r}; known: {', '.join(sorted(SCENES))}") from None
