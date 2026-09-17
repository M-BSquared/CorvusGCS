"""The flown track, as a function of time.

The map screenshots are the ones that cannot be faked with a static state: the
flown track is accumulated *in the browser* from position updates, so a picture
of a track is a picture of an aircraft that actually flew it. This module is
the flight: a path, a speed, and a model that answers "where is the aircraft,
which way is it pointing, and how far is the battery down" at any moment.

The paths are the shapes a test flight actually makes — an orbit, a lawnmower
survey, a there-and-back leg, a figure of eight — so a README picture shows a
sortie rather than a doodle. They are generated around a home point, not stored
as coordinates, which is what lets a scene be re-flown anywhere: change the home
point and the same survey lands on the new site.

Geometry is a local flat-earth approximation around home. Over the few hundred
metres a test site spans, the error is centimetres — far below what a track
drawn at 1 m decimation can show — and it keeps the whole model to arithmetic
that can be read.
"""
from __future__ import annotations

import math
from dataclasses import dataclass, field
from typing import Callable, Iterable

# Neubiberg: the test site the README's imagery is from, and the frontend's own
# default map centre (src/js/map.js). [lat, lon] here — the human order; the
# map's [lng, lat] conversion happens where it talks to MapLibre.
NEUBIBERG = (48.080217, 11.640969)

EARTH_R = 6378137.0
G = 9.80665


def _metres_per_degree(lat: float) -> tuple[float, float]:
    """(north, east) metres per degree of latitude/longitude at *lat*."""
    lat_rad = math.radians(lat)
    north = 111132.92 - 559.82 * math.cos(2 * lat_rad) + 1.175 * math.cos(4 * lat_rad)
    east = 111412.84 * math.cos(lat_rad) - 93.5 * math.cos(3 * lat_rad)
    return north, east


@dataclass(frozen=True)
class Point:
    """One point on the path: a position and the altitude to hold there."""

    lat: float
    lon: float
    alt: float            # metres above the launch point (AGL, as PX4 reports)


@dataclass
class Path:
    """A flight path and how it is flown."""

    name: str
    points: list[Point]
    speed: float = 8.0        # m/s along the ground
    loop: bool = True         # fly it again from the start when it ends
    hold_at_end: float = 0.0  # seconds to hover at the last point before looping

    def __post_init__(self) -> None:
        if len(self.points) < 2:
            raise ValueError("a path needs at least two points")


# ---------------------------------------------------------------------------
# Path generators
# ---------------------------------------------------------------------------


def _offset(home: tuple[float, float], north: float, east: float) -> tuple[float, float]:
    """*home* displaced by *north*/*east* metres."""
    mn, me = _metres_per_degree(home[0])
    return home[0] + north / mn, home[1] + east / me


def hover(home: tuple[float, float] = NEUBIBERG, alt: float = 30.0,
          drift: float = 3.0) -> Path:
    """Station-keeping: a few metres of drift, the way a real hover holds."""
    pts = []
    for i in range(12):
        angle = 2 * math.pi * i / 12
        lat, lon = _offset(home, drift * math.sin(angle), drift * math.cos(angle))
        pts.append(Point(lat, lon, alt))
    return Path("hover", pts, speed=1.2, loop=True)


def orbit(home: tuple[float, float] = NEUBIBERG, radius: float = 120.0,
          alt: float = 60.0, points: int = 48, clockwise: bool = True) -> Path:
    """A circle around home — the inspection orbit, and the cleanest track."""
    pts = []
    for i in range(points):
        angle = 2 * math.pi * i / points
        if clockwise:
            angle = -angle
        lat, lon = _offset(home, radius * math.cos(angle), radius * math.sin(angle))
        pts.append(Point(lat, lon, alt))
    return Path("orbit", pts, speed=9.0, loop=True)


def survey(home: tuple[float, float] = NEUBIBERG, width: float = 300.0,
           height: float = 200.0, spacing: float = 45.0, alt: float = 80.0,
           heading: float = 0.0) -> Path:
    """A lawnmower survey: the shape a mapping sortie leaves on the map.

    *heading* rotates the whole pattern, because a survey is flown along the
    field, not along north.
    """
    legs = max(2, int(height / spacing) + 1)
    rot = math.radians(heading)
    pts = []
    for i in range(legs):
        y = -height / 2 + i * (height / (legs - 1))
        xs = (-width / 2, width / 2) if i % 2 == 0 else (width / 2, -width / 2)
        for x in xs:
            north = x * math.sin(rot) + y * math.cos(rot)
            east = x * math.cos(rot) - y * math.sin(rot)
            lat, lon = _offset(home, north, east)
            pts.append(Point(lat, lon, alt))
    return Path("survey", pts, speed=12.0, loop=True)


def figure_eight(home: tuple[float, float] = NEUBIBERG, size: float = 150.0,
                 alt: float = 50.0, points: int = 64) -> Path:
    """A lemniscate — the shape a compass calibration flight or a demo makes."""
    pts = []
    for i in range(points):
        t = 2 * math.pi * i / points
        denom = 1 + math.sin(t) ** 2
        north = size * math.cos(t) / denom
        east = size * math.sin(t) * math.cos(t) / denom
        lat, lon = _offset(home, north, east)
        pts.append(Point(lat, lon, alt))
    return Path("figure_eight", pts, speed=10.0, loop=True)


def out_and_back(home: tuple[float, float] = NEUBIBERG, distance: float = 400.0,
                 bearing: float = 45.0, alt: float = 70.0) -> Path:
    """Out to a point and home again — the range check, and a track with a turn."""
    rad = math.radians(bearing)
    far = _offset(home, distance * math.cos(rad), distance * math.sin(rad))
    mid = _offset(home, distance * 0.5 * math.cos(rad) + 60 * math.sin(rad),
                  distance * 0.5 * math.sin(rad) - 60 * math.cos(rad))
    return Path("out_and_back", [
        Point(home[0], home[1], alt),
        Point(mid[0], mid[1], alt),
        Point(far[0], far[1], alt),
        Point(mid[0], mid[1], alt * 0.7),
        Point(home[0], home[1], alt * 0.7),
    ], speed=14.0, loop=True)


def waypoints(coords: Iterable[Iterable[float]], speed: float = 10.0,
              loop: bool = True) -> Path:
    """A path from explicit ``[lat, lon, alt]`` triples — for a real sortie."""
    pts = [Point(float(c[0]), float(c[1]), float(c[2]) if len(tuple(c)) > 2 else 50.0)
           for c in (tuple(x) for x in coords)]
    return Path("waypoints", pts, speed=speed, loop=loop)


PATHS: dict[str, Callable[..., Path]] = {
    "hover": hover,
    "orbit": orbit,
    "survey": survey,
    "figure_eight": figure_eight,
    "out_and_back": out_and_back,
}


def make_path(kind: str, home: tuple[float, float] = NEUBIBERG,
              **kwargs) -> Path:
    """Build a named path around *home*, passing *kwargs* to its generator."""
    if kind not in PATHS:
        raise KeyError(f"unknown path {kind!r}; known: {', '.join(sorted(PATHS))}")
    return PATHS[kind](home=home, **kwargs)


# ---------------------------------------------------------------------------
# Flying it
# ---------------------------------------------------------------------------


@dataclass
class Fix:
    """Everything the telemetry stream needs about one instant of flight."""

    lat: float
    lon: float
    alt_agl: float
    heading: float          # degrees true
    ground_speed: float     # m/s
    climb: float            # m/s, positive up
    roll: float             # radians
    pitch: float
    yaw: float
    vx: float               # NED components, m/s
    vy: float
    vz: float
    phase: str = "cruise"   # takeoff | cruise | landing | landed


@dataclass
class FlightModel:
    """Fly *path* from *home*, and answer where the aircraft is at time t.

    The flight has a shape as well as a track: it lifts off, climbs to the
    path's altitude, flies the path, and — when the scene asks for it — comes
    home and lands. A screenshot of a hovering aircraft that never took off is
    the one thing the HUD makes obvious, because the climb rate and the landed
    state are on screen next to each other.
    """

    path: Path
    home: tuple[float, float] = NEUBIBERG
    takeoff_time: float = 12.0     # seconds spent climbing before the path starts
    battery_capacity_ah: float = 16.0
    cruise_current: float = 38.0   # amps, a 500-class quad under way
    cells: int = 6
    _legs: list[tuple[Point, Point, float]] = field(default_factory=list, init=False)
    _length: float = field(default=0.0, init=False)

    def __post_init__(self) -> None:
        pts = list(self.path.points)
        if self.path.loop:
            pts.append(pts[0])
        mn, me = _metres_per_degree(self.home[0])
        for a, b in zip(pts, pts[1:]):
            dn = (b.lat - a.lat) * mn
            de = (b.lon - a.lon) * me
            self._legs.append((a, b, math.hypot(dn, de)))
        self._length = sum(leg[2] for leg in self._legs)

    # -- geometry ---------------------------------------------------------

    def _bearing(self, a: Point, b: Point) -> float:
        mn, me = _metres_per_degree(self.home[0])
        dn = (b.lat - a.lat) * mn
        de = (b.lon - a.lon) * me
        return math.degrees(math.atan2(de, dn)) % 360.0

    @property
    def duration(self) -> float:
        """Seconds for one lap of the path, excluding the takeoff."""
        return self._length / max(0.1, self.path.speed) + self.path.hold_at_end

    def _at_distance(self, dist: float) -> tuple[Point, Point, float]:
        """The leg *dist* metres along the path, and how far into it we are."""
        remaining = dist % max(1e-6, self._length)
        for a, b, length in self._legs:
            if remaining <= length or length <= 0:
                return a, b, (remaining / length if length > 0 else 0.0)
            remaining -= length
        a, b, length = self._legs[-1]
        return a, b, 1.0

    # -- the model --------------------------------------------------------

    def sample(self, t: float) -> Fix:
        """The aircraft's state *t* seconds into the flight."""
        first = self.path.points[0]
        if t < self.takeoff_time:
            # Climbing at the launch point. PX4 reports AGL, so this is the
            # number the HUD's altitude tape shows winding up.
            frac = max(0.0, t / max(0.1, self.takeoff_time))
            alt = first.alt * _ease(frac)
            climb = (first.alt / self.takeoff_time) * (1 - abs(2 * frac - 1)) * 1.5
            return Fix(
                lat=self.home[0], lon=self.home[1], alt_agl=round(alt, 2),
                heading=self._bearing(first, self.path.points[1]),
                ground_speed=0.4 * frac, climb=round(climb, 2),
                roll=0.0, pitch=math.radians(-1.5),
                yaw=math.radians(self._bearing(first, self.path.points[1])),
                vx=0.0, vy=0.0, vz=round(-climb, 2),
                phase="takeoff" if t > 0.5 else "landed",
            )

        flown = (t - self.takeoff_time) * self.path.speed
        a, b, frac = self._at_distance(flown)
        lat = a.lat + (b.lat - a.lat) * frac
        lon = a.lon + (b.lon - a.lon) * frac
        alt = a.alt + (b.alt - a.alt) * frac
        heading = self._bearing(a, b)

        # Bank angle from the turn the aircraft is in: the heading a second
        # from now against the heading now. A multirotor banks into its turn
        # like anything else, and a track drawn with the HUD level through a
        # 90-degree corner is the detail that gives a staged screenshot away.
        ahead_a, ahead_b, _ = self._at_distance(flown + self.path.speed)
        turn = _angle_diff(self._bearing(ahead_a, ahead_b), heading)
        roll = math.atan((math.radians(turn) * self.path.speed) / G)
        climb = (b.alt - a.alt) / max(1.0, self.path.speed)

        rad = math.radians(heading)
        return Fix(
            lat=lat, lon=lon, alt_agl=round(alt, 2), heading=heading,
            ground_speed=self.path.speed, climb=round(climb, 2),
            roll=round(max(-0.6, min(0.6, roll)), 4),
            # Nose down into the airflow, harder the faster it is going.
            pitch=round(-math.radians(2 + self.path.speed * 0.6), 4),
            yaw=math.radians(heading),
            vx=round(self.path.speed * math.cos(rad), 2),
            vy=round(self.path.speed * math.sin(rad), 2),
            vz=round(-climb, 2),
            phase="cruise",
        )

    def battery(self, t: float) -> tuple[float, float, int]:
        """(volts, amps, remaining %) after *t* seconds of flight.

        A lithium pack's voltage sags under load and recovers when it is not:
        the curve below is the flat middle of a 6S discharge, not a straight
        line from full to empty, because the top-bar voltage reading is the
        number an operator checks first.
        """
        drawn_ah = self.cruise_current * max(0.0, t) / 3600.0
        used = min(1.0, drawn_ah / self.battery_capacity_ah)
        per_cell = 4.15 - 0.55 * used - 0.25 * (used ** 6)
        current = self.cruise_current * (0.35 if t < self.takeoff_time else 1.0)
        return (round(per_cell * self.cells, 2), round(current, 1),
                int(round((1.0 - used) * 100)))


def _ease(x: float) -> float:
    """Smoothstep — a climb that starts and stops instead of stepping."""
    x = max(0.0, min(1.0, x))
    return x * x * (3 - 2 * x)


def _angle_diff(a: float, b: float) -> float:
    """Signed smallest difference *a* - *b*, in degrees."""
    return (a - b + 180.0) % 360.0 - 180.0
