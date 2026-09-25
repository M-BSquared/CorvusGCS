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
# The short hop across the field on the Neubiberg campus that every flying
# picture shows: from the start point to the end point, along a gentle curve.
CAMPUS_HOP_START = (48.075718, 11.642051)
CAMPUS_HOP_END = (48.076507, 11.644507)
# How far the hop's curve sags right of the straight line at its middle, as a
# fraction of the line's length. A fraction rather than metres, so the curve
# keeps its shape whichever two points it joins.
HOP_SAG = 0.084

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


def parked(home: tuple[float, float] = NEUBIBERG, alt: float = 0.0) -> Path:
    """On the ground at the start point, disarmed: the configuration pages.

    A path needs a length, so this one has a few centimetres, crept round
    very slowly; the model reports it as standing still and landed.
    """
    pts = []
    for i in range(6):
        angle = 2 * math.pi * i / 6
        lat, lon = _offset(home, 0.02 * math.sin(angle), 0.02 * math.cos(angle))
        pts.append(Point(lat, lon, alt))
    return Path("parked", pts, speed=0.002, loop=True)


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
                 alt: float = 50.0, points: int = 720) -> Path:
    """A lemniscate — the shape a compass calibration flight or a demo makes.

    Finely divided because the heading turns at every vertex: at 64 points the
    angular rates the tuning charts draw were flat lines with a 150 deg/s
    spike at each corner, where a real figure of eight turns smoothly.
    """
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


def swoop(home: tuple[float, float] = CAMPUS_HOP_START,
          end: Iterable[float] = CAMPUS_HOP_END, bulge: float | None = None,
          alt: float = 40.0, speed: float = 6.0, points: int = 24) -> Path:
    """From *home* to *end* along a gentle curve, flown once and held there.

    The curve is a quadratic Bezier that passes *bulge* metres to the left of
    the straight line at its midpoint; a negative *bulge* swings it right.

    The default is the track the README's pictures show: leaving the start
    point nearly due east, sagging south of the straight line by
    :data:`HOP_SAG` of its length, and turning north-east into the end point,
    so the aircraft arrives pointing further north than the line it flew
    along.
    """
    end = tuple(end)
    mn, me = _metres_per_degree(home[0])
    dn = (end[0] - home[0]) * mn
    de = (end[1] - home[1]) * me
    length = math.hypot(dn, de) or 1.0
    if bulge is None:
        bulge = -HOP_SAG * length
    cn = dn / 2 + 2 * bulge * de / length
    ce = de / 2 - 2 * bulge * dn / length
    pts = []
    for i in range(points + 1):
        u = i / points
        north = 2 * (1 - u) * u * cn + u * u * dn
        east = 2 * (1 - u) * u * ce + u * u * de
        lat, lon = _offset(home, north, east)
        pts.append(Point(lat, lon, alt))
    return Path("swoop", pts, speed=speed, loop=False)


def waypoints(coords: Iterable[Iterable[float]], speed: float = 10.0,
              loop: bool = True) -> Path:
    """A path from explicit ``[lat, lon, alt]`` triples — for a real sortie."""
    pts = [Point(float(c[0]), float(c[1]), float(c[2]) if len(tuple(c)) > 2 else 50.0)
           for c in (tuple(x) for x in coords)]
    return Path("waypoints", pts, speed=speed, loop=loop)


PATHS: dict[str, Callable[..., Path]] = {
    "parked": parked,
    "hover": hover,
    "orbit": orbit,
    "survey": survey,
    "figure_eight": figure_eight,
    "out_and_back": out_and_back,
    "swoop": swoop,
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

    def _position(self, dist: float) -> tuple[float, float, float]:
        """(lat, lon, alt) *dist* metres along the path, clamped to a path that ends."""
        if not self.path.loop:
            dist = min(max(dist, 0.0), self._length * (1 - 1e-9))
        a, b, frac = self._at_distance(dist)
        return (a.lat + (b.lat - a.lat) * frac, a.lon + (b.lon - a.lon) * frac,
                a.alt + (b.alt - a.alt) * frac)

    def _tangent(self, dist: float, span: float = 1.5) -> float:
        """The path's heading at *dist*, from a chord a few metres across it.

        A leg's own bearing steps at every vertex, and a heading that steps is
        a yaw rate and a bank angle that jitter from one leg to the next: the
        tuning charts drew that as noise no airframe makes. The chord turns
        smoothly through a vertex instead.
        """
        la, loa, _ = self._position(dist - span)
        lb, lob, _ = self._position(dist + span)
        mn, me = _metres_per_degree(self.home[0])
        dn, de = (lb - la) * mn, (lob - loa) * me
        if abs(dn) + abs(de) < 1e-9:
            a, b, _ = self._at_distance(dist)
            return self._bearing(a, b)
        return math.degrees(math.atan2(de, dn)) % 360.0

    def leg_at(self, t: float) -> int:
        """Which leg of the path is being flown *t* seconds in: 0 is the first.

        The climb counts as the approach to the first point, so this is also
        "the point being flown to, minus one" for a mission's MISSION_CURRENT.
        """
        if t < self.takeoff_time or not self._legs:
            return -1
        remaining = ((t - self.takeoff_time) * max(0.1, self.path.speed)) % max(1e-6, self._length)
        for index, (_a, _b, length) in enumerate(self._legs):
            if remaining <= length:
                return index
            remaining -= length
        return len(self._legs) - 1

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

        if self.path.name == "parked":
            return Fix(
                lat=first.lat, lon=first.lon, alt_agl=0.0,
                heading=62.0, ground_speed=0.0, climb=0.0,
                roll=0.004, pitch=-0.006, yaw=math.radians(62.0),
                vx=0.0, vy=0.0, vz=0.0, phase="landed",
            )
        flown = (t - self.takeoff_time) * self.path.speed
        # A path that does not loop is flown once, then held at its end.
        arrived = not self.path.loop and flown >= self._length
        if arrived:
            flown = self._length * (1 - 1e-9)
        speed = 0.0 if arrived else self.path.speed
        a, b, frac = self._at_distance(flown)
        lat = a.lat + (b.lat - a.lat) * frac
        lon = a.lon + (b.lon - a.lon) * frac
        alt = a.alt + (b.alt - a.alt) * frac
        heading = self._tangent(flown)

        # Bank angle from the turn the aircraft is in: the heading half a
        # second ahead against half a second back. A multirotor banks into its
        # turn like anything else, and a track drawn with the HUD level through
        # a 90-degree corner is the detail that gives a staged screenshot away.
        turn = _angle_diff(self._tangent(flown + speed * 0.5),
                           self._tangent(flown - speed * 0.5))
        roll = math.atan((math.radians(turn) * speed) / G)
        climb = (b.alt - a.alt) / max(1.0, self.path.speed)

        rad = math.radians(heading)
        return Fix(
            lat=lat, lon=lon, alt_agl=round(alt, 2), heading=heading,
            ground_speed=speed, climb=round(climb, 2),
            roll=round(max(-0.6, min(0.6, roll)), 4),
            # Nose down into the airflow, harder the faster it is going.
            pitch=round(-math.radians(2 + speed * 0.6), 4),
            yaw=math.radians(heading),
            vx=round(speed * math.cos(rad), 2),
            vy=round(speed * math.sin(rad), 2),
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
        if self.path.name == "parked":
            # Disarmed on the ground: the flight controller, the receiver and
            # the companion computer, and no motors.
            current = 0.9
            drawn_ah = current * max(0.0, t) / 3600.0
        else:
            drawn_ah = self.cruise_current * max(0.0, t) / 3600.0
            current = self.cruise_current * (0.35 if t < self.takeoff_time else 1.0)
        used = min(1.0, drawn_ah / self.battery_capacity_ah)
        per_cell = 4.15 - 0.55 * used - 0.25 * (used ** 6)
        return (round(per_cell * self.cells, 2), round(current, 1),
                int(round((1.0 - used) * 100)))


def _ease(x: float) -> float:
    """Smoothstep — a climb that starts and stops instead of stepping."""
    x = max(0.0, min(1.0, x))
    return x * x * (3 - 2 * x)


def _angle_diff(a: float, b: float) -> float:
    """Signed smallest difference *a* - *b*, in degrees."""
    return (a - b + 180.0) % 360.0 - 180.0
