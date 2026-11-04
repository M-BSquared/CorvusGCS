"""Coordinate systems an operator's own elevation data arrives in.

A digital surface model from a survey office is almost never in longitude and
latitude. Germany publishes in ETRS89 / UTM, Switzerland in LV95, France in
Lambert-93, Great Britain in the National Grid, and older German data in
Gauss-Krüger on the Bessel ellipsoid. To drape such a grid under the map, every
tile pixel has to be taken from Web Mercator back into the grid's own system,
so each supported system needs both directions.

Only the systems below are known, by EPSG code. A file in another one is
refused with the list, never guessed at: a grid placed in the wrong system is
a hill drawn kilometres from where it stands, and nothing on the map would say
so.

Accuracy is what a terrain mesh needs, not what a survey needs: well under a
metre for the projections themselves, and a few metres for the old national
datums (DHDN, OSGB36), whose shift to WGS84 is a single seven parameter
transformation here rather than the national grid files. ETRS89 is taken as
WGS84, which it differs from by well under a metre in Europe.

stdlib only, pure functions. :func:`get` returns a :class:`Crs` whose
``to_lonlat`` and ``from_lonlat`` take and return plain floats.
"""
from __future__ import annotations

import math
from collections.abc import Callable
from dataclasses import dataclass

_Fn = Callable[[float, float], "tuple[float, float]"]


@dataclass(frozen=True)
class Crs:
    """One coordinate system: its EPSG code, a name, and both directions."""

    epsg: int
    name: str
    geographic: bool
    to_lonlat: _Fn
    from_lonlat: _Fn


# (semi-major axis, inverse flattening)
_WGS84 = (6378137.0, 298.257223563)
_GRS80 = (6378137.0, 298.257222101)
_BESSEL = (6377397.155, 299.1528128)
_AIRY = (6377563.396, 299.3249646)

# PROJ's +towgs84 seven parameters (position vector): metres, arc seconds, ppm.
_DHDN_TO_WGS84 = (598.1, 73.7, 418.2, 0.202, 0.045, -2.455, 6.7)
_OSGB36_TO_WGS84 = (446.448, -125.157, 542.06, 0.15, 0.247, 0.842, -20.489)


def _ecc(ellipsoid: tuple[float, float]) -> tuple[float, float, float]:
    a, inv_f = ellipsoid
    f = 1.0 / inv_f
    e2 = f * (2.0 - f)
    return a, f, e2


class _TransverseMercator:
    """Transverse Mercator by the Krüger series to n**3.

    Better than a millimetre within a UTM zone, and still far better than a
    terrain pixel several degrees outside one, which is why one formula
    serves UTM, Gauss-Krüger and the British grid alike.
    """

    def __init__(self, ellipsoid: tuple[float, float], lon0: float, k0: float,
                 false_e: float, false_n: float, lat0: float = 0.0) -> None:
        a, f, e2 = _ecc(ellipsoid)
        n = f / (2.0 - f)
        self.e = math.sqrt(e2)
        self.lon0 = math.radians(lon0)
        self.k0 = k0
        self.fe = false_e
        self.A = a / (1.0 + n) * (1.0 + n * n / 4.0 + n ** 4 / 64.0)
        self.alpha = (n / 2.0 - 2.0 * n * n / 3.0 + 5.0 * n ** 3 / 16.0,
                      13.0 * n * n / 48.0 - 3.0 * n ** 3 / 5.0,
                      61.0 * n ** 3 / 240.0)
        self.beta = (n / 2.0 - 2.0 * n * n / 3.0 + 37.0 * n ** 3 / 96.0,
                     n * n / 48.0 + n ** 3 / 15.0,
                     17.0 * n ** 3 / 480.0)
        self.delta = (2.0 * n - 2.0 * n * n / 3.0 - 2.0 * n ** 3,
                      7.0 * n * n / 3.0 - 8.0 * n ** 3 / 5.0,
                      56.0 * n ** 3 / 15.0)
        self.fn = 0.0
        if lat0:
            self.fn = false_n - self._forward(math.radians(lat0), self.lon0)[1]
        else:
            self.fn = false_n

    def _forward(self, phi: float, lam: float) -> tuple[float, float]:
        e = self.e
        s = math.sin(phi)
        t = math.sinh(math.atanh(s) - e * math.atanh(e * s))
        dl = lam - self.lon0
        xi = math.atan2(t, math.cos(dl))
        eta = math.atanh(math.sin(dl) / math.sqrt(1.0 + t * t))
        x = eta
        y = xi
        for j, a in enumerate(self.alpha, 1):
            x += a * math.cos(2 * j * xi) * math.sinh(2 * j * eta)
            y += a * math.sin(2 * j * xi) * math.cosh(2 * j * eta)
        return self.fe + self.k0 * self.A * x, self.k0 * self.A * y

    def forward(self, lon: float, lat: float) -> tuple[float, float]:
        e, n = self._forward(math.radians(lat), math.radians(lon))
        return e, n + self.fn

    def inverse(self, east: float, north: float) -> tuple[float, float]:
        xi = (north - self.fn) / (self.k0 * self.A)
        eta = (east - self.fe) / (self.k0 * self.A)
        xp = xi
        ep = eta
        for j, b in enumerate(self.beta, 1):
            xp -= b * math.sin(2 * j * xi) * math.cosh(2 * j * eta)
            ep -= b * math.cos(2 * j * xi) * math.sinh(2 * j * eta)
        chi = math.asin(max(-1.0, min(1.0, math.sin(xp) / math.cosh(ep))))
        phi = chi
        for j, d in enumerate(self.delta, 1):
            phi += d * math.sin(2 * j * chi)
        lam = self.lon0 + math.atan2(math.sinh(ep), math.cos(xp))
        return math.degrees(lam), math.degrees(phi)


class _LambertConic:
    """Lambert Conformal Conic with two standard parallels (EPSG 9802)."""

    def __init__(self, ellipsoid: tuple[float, float], lat1: float, lat2: float,
                 lat0: float, lon0: float, false_e: float, false_n: float) -> None:
        a, _f, e2 = _ecc(ellipsoid)
        self.a = a
        self.e = math.sqrt(e2)
        self.lon0 = math.radians(lon0)
        self.fe = false_e
        self.fn = false_n
        p1, p2, p0 = (math.radians(v) for v in (lat1, lat2, lat0))
        m1, m2 = self._m(p1), self._m(p2)
        t1, t2, t0 = self._t(p1), self._t(p2), self._t(p0)
        self.n = (math.log(m1) - math.log(m2)) / (math.log(t1) - math.log(t2))
        self.F = m1 / (self.n * t1 ** self.n)
        self.rho0 = a * self.F * t0 ** self.n

    def _m(self, phi: float) -> float:
        s = math.sin(phi)
        return math.cos(phi) / math.sqrt(1.0 - self.e * self.e * s * s)

    def _t(self, phi: float) -> float:
        s = math.sin(phi)
        e = self.e
        return math.tan(math.pi / 4.0 - phi / 2.0) / ((1.0 - e * s) / (1.0 + e * s)) ** (e / 2.0)

    def forward(self, lon: float, lat: float) -> tuple[float, float]:
        phi = math.radians(lat)
        rho = self.a * self.F * self._t(phi) ** self.n
        theta = self.n * (math.radians(lon) - self.lon0)
        return self.fe + rho * math.sin(theta), self.fn + self.rho0 - rho * math.cos(theta)

    def inverse(self, east: float, north: float) -> tuple[float, float]:
        dx = east - self.fe
        dy = self.rho0 - (north - self.fn)
        rho = math.copysign(math.hypot(dx, dy), self.n)
        t = (rho / (self.a * self.F)) ** (1.0 / self.n)
        theta = math.atan2(dx, dy)
        e = self.e
        phi = math.pi / 2.0 - 2.0 * math.atan(t)
        for _ in range(8):
            s = math.sin(phi)
            phi = math.pi / 2.0 - 2.0 * math.atan(
                t * ((1.0 - e * s) / (1.0 + e * s)) ** (e / 2.0))
        return math.degrees(theta / self.n + self.lon0), math.degrees(phi)


class _LambertAzimuthal:
    """Lambert Azimuthal Equal Area, ellipsoidal (EPSG 9820)."""

    def __init__(self, ellipsoid: tuple[float, float], lat0: float, lon0: float,
                 false_e: float, false_n: float) -> None:
        a, _f, e2 = _ecc(ellipsoid)
        self.a = a
        self.e2 = e2
        self.e = math.sqrt(e2)
        self.lon0 = math.radians(lon0)
        self.fe = false_e
        self.fn = false_n
        p0 = math.radians(lat0)
        self.qp = self._q(math.pi / 2.0)
        self.b0 = math.asin(self._q(p0) / self.qp)
        self.rq = a * math.sqrt(self.qp / 2.0)
        s0 = math.sin(p0)
        self.d = a * (math.cos(p0) / math.sqrt(1.0 - e2 * s0 * s0)) / (self.rq * math.cos(self.b0))

    def _q(self, phi: float) -> float:
        e, e2 = self.e, self.e2
        s = math.sin(phi)
        return (1.0 - e2) * (s / (1.0 - e2 * s * s)
                             - (1.0 / (2.0 * e)) * math.log((1.0 - e * s) / (1.0 + e * s)))

    def forward(self, lon: float, lat: float) -> tuple[float, float]:
        beta = math.asin(max(-1.0, min(1.0, self._q(math.radians(lat)) / self.qp)))
        dl = math.radians(lon) - self.lon0
        b0 = self.b0
        b = self.rq * math.sqrt(2.0 / (1.0 + math.sin(b0) * math.sin(beta)
                                       + math.cos(b0) * math.cos(beta) * math.cos(dl)))
        east = self.fe + b * self.d * math.cos(beta) * math.sin(dl)
        north = self.fn + (b / self.d) * (math.cos(b0) * math.sin(beta)
                                          - math.sin(b0) * math.cos(beta) * math.cos(dl))
        return east, north

    def inverse(self, east: float, north: float) -> tuple[float, float]:
        x = east - self.fe
        y = north - self.fn
        d, b0 = self.d, self.b0
        rho = math.hypot(x / d, d * y)
        if rho < 1e-9:
            beta, lam = b0, self.lon0
        else:
            c = 2.0 * math.asin(min(1.0, rho / (2.0 * self.rq)))
            beta = math.asin(math.cos(c) * math.sin(b0) + d * y * math.sin(c) * math.cos(b0) / rho)
            lam = self.lon0 + math.atan2(x * math.sin(c),
                                         d * rho * math.cos(b0) * math.cos(c)
                                         - d * d * y * math.sin(b0) * math.sin(c))
        e2 = self.e2
        phi = (beta
               + (e2 / 3.0 + 31.0 * e2 ** 2 / 180.0 + 517.0 * e2 ** 3 / 5040.0) * math.sin(2 * beta)
               + (23.0 * e2 ** 2 / 360.0 + 251.0 * e2 ** 3 / 3780.0) * math.sin(4 * beta)
               + (761.0 * e2 ** 3 / 45360.0) * math.sin(6 * beta))
        return math.degrees(lam), math.degrees(phi)


def _geocentric(lon: float, lat: float, ellipsoid: tuple[float, float]) -> tuple[float, float, float]:
    a, _f, e2 = _ecc(ellipsoid)
    phi, lam = math.radians(lat), math.radians(lon)
    s = math.sin(phi)
    nu = a / math.sqrt(1.0 - e2 * s * s)
    return (nu * math.cos(phi) * math.cos(lam),
            nu * math.cos(phi) * math.sin(lam),
            nu * (1.0 - e2) * s)


def _geodetic(x: float, y: float, z: float, ellipsoid: tuple[float, float]) -> tuple[float, float]:
    a, _f, e2 = _ecc(ellipsoid)
    p = math.hypot(x, y)
    phi = math.atan2(z, p * (1.0 - e2))
    for _ in range(6):
        s = math.sin(phi)
        nu = a / math.sqrt(1.0 - e2 * s * s)
        phi = math.atan2(z + e2 * nu * s, p)
    return math.degrees(math.atan2(y, x)), math.degrees(phi)


def _helmert(xyz: tuple[float, float, float], params: tuple[float, ...],
             sign: float) -> tuple[float, float, float]:
    """Seven parameter shift, position vector convention; *sign* -1 undoes it."""
    tx, ty, tz, rx, ry, rz, ds = (sign * v for v in params)
    k = math.pi / 648000.0
    rx, ry, rz = rx * k, ry * k, rz * k
    m = 1.0 + ds * 1e-6
    x, y, z = xyz
    return (tx + m * (x - rz * y + ry * z),
            ty + m * (rz * x + y - rx * z),
            tz + m * (-ry * x + rx * y + z))


def _datum(ellipsoid: tuple[float, float], params: tuple[float, ...]) -> tuple[_Fn, _Fn]:
    """(local lon/lat -> WGS84, WGS84 -> local) for a shifted datum."""
    def to_wgs(lon: float, lat: float) -> tuple[float, float]:
        return _geodetic(*_helmert(_geocentric(lon, lat, ellipsoid), params, 1.0), _WGS84)

    def from_wgs(lon: float, lat: float) -> tuple[float, float]:
        return _geodetic(*_helmert(_geocentric(lon, lat, _WGS84), params, -1.0), ellipsoid)

    return to_wgs, from_wgs


def _projected(epsg: int, name: str, proj, datum: tuple[_Fn, _Fn] | None = None) -> Crs:
    if datum is None:
        return Crs(epsg, name, False, proj.inverse, proj.forward)
    to_wgs, from_wgs = datum
    return Crs(epsg, name, False,
               lambda e, n: to_wgs(*proj.inverse(e, n)),
               lambda lon, lat: proj.forward(*from_wgs(lon, lat)))


def _identity(lon: float, lat: float) -> tuple[float, float]:
    return lon, lat


_MERC_R = 6378137.0


def _merc_inverse(x: float, y: float) -> tuple[float, float]:
    return (math.degrees(x / _MERC_R),
            math.degrees(2.0 * math.atan(math.exp(y / _MERC_R)) - math.pi / 2.0))


def _merc_forward(lon: float, lat: float) -> tuple[float, float]:
    lat = max(-85.051129, min(85.051129, lat))
    return (math.radians(lon) * _MERC_R,
            math.log(math.tan(math.pi / 4.0 + math.radians(lat) / 2.0)) * _MERC_R)


# Swiss LV95 / LV03 by swisstopo's published approximate formulas, which are
# good to about a metre across the country.
def _swiss(e_off: float, n_off: float) -> tuple[_Fn, _Fn]:
    def to_lonlat(east: float, north: float) -> tuple[float, float]:
        y = (east - e_off - 600000.0) / 1e6
        x = (north - n_off - 200000.0) / 1e6
        lam = (2.6779094 + 4.728982 * y + 0.791484 * y * x + 0.1306 * y * x * x
               - 0.0436 * y ** 3)
        phi = (16.9023892 + 3.238272 * x - 0.270978 * y * y - 0.002528 * x * x
               - 0.0447 * y * y * x - 0.0140 * x ** 3)
        return lam * 100.0 / 36.0, phi * 100.0 / 36.0

    def from_lonlat(lon: float, lat: float) -> tuple[float, float]:
        p = (lat * 3600.0 - 169028.66) / 10000.0
        lam = (lon * 3600.0 - 26782.5) / 10000.0
        east = (600072.37 + 211455.93 * lam - 10938.51 * lam * p - 0.36 * lam * p * p
                - 44.54 * lam ** 3)
        north = (200147.07 + 308807.95 * p + 3745.25 * lam * lam + 76.63 * p * p
                 - 194.56 * lam * lam * p + 119.79 * p ** 3)
        return east + e_off, north + n_off

    return to_lonlat, from_lonlat


def _utm(zone: int, south: bool, ellipsoid: tuple[float, float], false_e: float = 500000.0):
    return _TransverseMercator(ellipsoid, zone * 6.0 - 183.0, 0.9996, false_e,
                               10000000.0 if south else 0.0)


def get(epsg: int) -> Crs | None:
    """The coordinate system with EPSG code *epsg*, or None when unsupported."""
    try:
        code = int(epsg)
    except (TypeError, ValueError):
        return None
    if code in (4326, 4258, 4269, 4283, 4167, 4617):
        return Crs(code, "Longitude and latitude", True, _identity, _identity)
    if code in (3857, 900913, 102100, 102113):
        return Crs(code, "Web Mercator", False, _merc_inverse, _merc_forward)
    if 32601 <= code <= 32660:
        zone = code - 32600
        return _projected(code, f"WGS 84 / UTM zone {zone}N", _utm(zone, False, _WGS84))
    if 32701 <= code <= 32760:
        zone = code - 32700
        return _projected(code, f"WGS 84 / UTM zone {zone}S", _utm(zone, True, _WGS84))
    if 25828 <= code <= 25838:
        zone = code - 25800
        return _projected(code, f"ETRS89 / UTM zone {zone}N", _utm(zone, False, _GRS80))
    if 26901 <= code <= 26923:
        zone = code - 26900
        return _projected(code, f"NAD83 / UTM zone {zone}N", _utm(zone, False, _GRS80))
    # ETRS89 / UTM with the zone number in front of the easting, as several
    # German states publish it (32 696000 rather than 696000).
    zone_prefixed = {5649: 31, 4647: 32, 5650: 33}
    if code in zone_prefixed:
        zone = zone_prefixed[code]
        return _projected(code, f"ETRS89 / UTM zone {zone}N (zone in easting)",
                          _utm(zone, False, _GRS80, zone * 1e6 + 500000.0))
    gk = {31466: 2, 31467: 3, 31468: 4, 31469: 5, 31492: 2, 31493: 3, 31494: 4, 31495: 5}
    if code in gk:
        zone = gk[code]
        proj = _TransverseMercator(_BESSEL, zone * 3.0, 1.0, zone * 1e6 + 500000.0, 0.0)
        return _projected(code, f"DHDN / Gauss-Krüger zone {zone}", proj,
                          _datum(_BESSEL, _DHDN_TO_WGS84))
    if code == 27700:
        proj = _TransverseMercator(_AIRY, -2.0, 0.9996012717, 400000.0, -100000.0, 49.0)
        return _projected(code, "OSGB36 / British National Grid", proj,
                          _datum(_AIRY, _OSGB36_TO_WGS84))
    if code == 2154:
        proj = _LambertConic(_GRS80, 49.0, 44.0, 46.5, 3.0, 700000.0, 6600000.0)
        return _projected(code, "RGF93 / Lambert-93", proj)
    if code == 3035:
        proj = _LambertAzimuthal(_GRS80, 52.0, 10.0, 4321000.0, 3210000.0)
        return _projected(code, "ETRS89 / LAEA Europe", proj)
    if code == 2056:
        to_ll, from_ll = _swiss(2000000.0, 1000000.0)
        return Crs(code, "CH1903+ / LV95", False, to_ll, from_ll)
    if code == 21781:
        to_ll, from_ll = _swiss(0.0, 0.0)
        return Crs(code, "CH1903 / LV03", False, to_ll, from_ll)
    return None


SUPPORTED_HINT = (
    "WGS 84 (4326), ETRS89 (4258), Web Mercator (3857), UTM on WGS 84 "
    "(32601 to 32660, 32701 to 32760), ETRS89 / UTM (25828 to 25838, and "
    "4647, 5649, 5650 with the zone in the easting), NAD83 / UTM (26901 to "
    "26923), DHDN / Gauss-Krüger (31466 to 31469), British National Grid "
    "(27700), Lambert-93 (2154), LAEA Europe (3035), Swiss LV95 (2056) and "
    "LV03 (21781)"
)
