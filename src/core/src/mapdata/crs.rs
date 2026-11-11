//! Coordinate systems an operator's own elevation data arrives in
//! (`corvus/crs.py`).
//!
//! A grid from a survey office is almost never in longitude and latitude, so
//! every tile pixel has to be taken from Web Mercator back into the grid's own
//! system and each supported system needs both directions. Only the systems
//! listed in [`get`] are known, by EPSG code: a file in another one is refused
//! with [`SUPPORTED_HINT`], never guessed at.

use std::f64::consts::PI;
use std::sync::Arc;

type Pair = (f64, f64);
type Func = Arc<dyn Fn(f64, f64) -> Pair + Send + Sync>;

/// One coordinate system: its EPSG code, a name and both directions.
#[derive(Clone)]
pub struct Crs {
    pub epsg: i64,
    pub name: String,
    pub geographic: bool,
    to_lonlat: Func,
    from_lonlat: Func,
}

impl Crs {
    /// Easting and northing (or lon and lat) to longitude and latitude.
    pub fn to_lonlat(&self, a: f64, b: f64) -> Pair {
        (self.to_lonlat)(a, b)
    }

    /// Longitude and latitude to this system's two axes.
    pub fn from_lonlat(&self, lon: f64, lat: f64) -> Pair {
        (self.from_lonlat)(lon, lat)
    }
}

impl std::fmt::Debug for Crs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Crs")
            .field("epsg", &self.epsg)
            .field("name", &self.name)
            .field("geographic", &self.geographic)
            .finish()
    }
}

/// (semi-major axis, inverse flattening)
type Ellipsoid = (f64, f64);
const WGS84: Ellipsoid = (6378137.0, 298.257223563);
const GRS80: Ellipsoid = (6378137.0, 298.257222101);
const BESSEL: Ellipsoid = (6377397.155, 299.1528128);
const AIRY: Ellipsoid = (6377563.396, 299.3249646);

/// PROJ's +towgs84 seven parameters (position vector): metres, arc seconds, ppm.
const DHDN_TO_WGS84: [f64; 7] = [598.1, 73.7, 418.2, 0.202, 0.045, -2.455, 6.7];
const OSGB36_TO_WGS84: [f64; 7] = [446.448, -125.157, 542.06, 0.15, 0.247, 0.842, -20.489];

fn ecc(e: Ellipsoid) -> (f64, f64, f64) {
    let (a, inv_f) = e;
    let f = 1.0 / inv_f;
    (a, f, f * (2.0 - f))
}

/// Transverse Mercator by the Krüger series to n^3.
struct TransverseMercator {
    e: f64,
    lon0: f64,
    k0: f64,
    fe: f64,
    a_big: f64,
    alpha: [f64; 3],
    beta: [f64; 3],
    delta: [f64; 3],
    fn_: f64,
}

impl TransverseMercator {
    fn new(ellipsoid: Ellipsoid, lon0: f64, k0: f64, false_e: f64, false_n: f64, lat0: f64) -> Self {
        let (a, f, e2) = ecc(ellipsoid);
        let n = f / (2.0 - f);
        let mut tm = TransverseMercator {
            e: e2.sqrt(),
            lon0: lon0.to_radians(),
            k0,
            fe: false_e,
            a_big: a / (1.0 + n) * (1.0 + n * n / 4.0 + n.powi(4) / 64.0),
            alpha: [
                n / 2.0 - 2.0 * n * n / 3.0 + 5.0 * n.powi(3) / 16.0,
                13.0 * n * n / 48.0 - 3.0 * n.powi(3) / 5.0,
                61.0 * n.powi(3) / 240.0,
            ],
            beta: [
                n / 2.0 - 2.0 * n * n / 3.0 + 37.0 * n.powi(3) / 96.0,
                n * n / 48.0 + n.powi(3) / 15.0,
                17.0 * n.powi(3) / 480.0,
            ],
            delta: [
                2.0 * n - 2.0 * n * n / 3.0 - 2.0 * n.powi(3),
                7.0 * n * n / 3.0 - 8.0 * n.powi(3) / 5.0,
                56.0 * n.powi(3) / 15.0,
            ],
            fn_: 0.0,
        };
        tm.fn_ = if lat0 != 0.0 {
            false_n - tm.forward_raw(lat0.to_radians(), tm.lon0).1
        } else {
            false_n
        };
        tm
    }

    fn forward_raw(&self, phi: f64, lam: f64) -> Pair {
        let e = self.e;
        let s = phi.sin();
        let t = (s.atanh() - e * (e * s).atanh()).sinh();
        let dl = lam - self.lon0;
        let xi = t.atan2(dl.cos());
        let eta = (dl.sin() / (1.0 + t * t).sqrt()).atanh();
        let (mut x, mut y) = (eta, xi);
        for (j, a) in self.alpha.iter().enumerate() {
            let j2 = 2.0 * (j as f64 + 1.0);
            x += a * (j2 * xi).cos() * (j2 * eta).sinh();
            y += a * (j2 * xi).sin() * (j2 * eta).cosh();
        }
        (self.fe + self.k0 * self.a_big * x, self.k0 * self.a_big * y)
    }

    fn forward(&self, lon: f64, lat: f64) -> Pair {
        let (e, n) = self.forward_raw(lat.to_radians(), lon.to_radians());
        (e, n + self.fn_)
    }

    fn inverse(&self, east: f64, north: f64) -> Pair {
        let xi = (north - self.fn_) / (self.k0 * self.a_big);
        let eta = (east - self.fe) / (self.k0 * self.a_big);
        let (mut xp, mut ep) = (xi, eta);
        for (j, b) in self.beta.iter().enumerate() {
            let j2 = 2.0 * (j as f64 + 1.0);
            xp -= b * (j2 * xi).sin() * (j2 * eta).cosh();
            ep -= b * (j2 * xi).cos() * (j2 * eta).sinh();
        }
        let chi = (xp.sin() / ep.cosh()).clamp(-1.0, 1.0).asin();
        let mut phi = chi;
        for (j, d) in self.delta.iter().enumerate() {
            phi += d * (2.0 * (j as f64 + 1.0) * chi).sin();
        }
        let lam = self.lon0 + ep.sinh().atan2(xp.cos());
        (lam.to_degrees(), phi.to_degrees())
    }
}

/// Lambert Conformal Conic with two standard parallels (EPSG 9802).
struct LambertConic {
    a: f64,
    e: f64,
    lon0: f64,
    fe: f64,
    fn_: f64,
    n: f64,
    f_big: f64,
    rho0: f64,
}

impl LambertConic {
    fn new(ellipsoid: Ellipsoid, lat1: f64, lat2: f64, lat0: f64, lon0: f64, fe: f64, fnn: f64) -> Self {
        let (a, _f, e2) = ecc(ellipsoid);
        let mut lc = LambertConic {
            a,
            e: e2.sqrt(),
            lon0: lon0.to_radians(),
            fe,
            fn_: fnn,
            n: 0.0,
            f_big: 0.0,
            rho0: 0.0,
        };
        let (p1, p2, p0) = (lat1.to_radians(), lat2.to_radians(), lat0.to_radians());
        let (m1, m2) = (lc.m(p1), lc.m(p2));
        let (t1, t2, t0) = (lc.t(p1), lc.t(p2), lc.t(p0));
        lc.n = (m1.ln() - m2.ln()) / (t1.ln() - t2.ln());
        lc.f_big = m1 / (lc.n * t1.powf(lc.n));
        lc.rho0 = a * lc.f_big * t0.powf(lc.n);
        lc
    }

    fn m(&self, phi: f64) -> f64 {
        let s = phi.sin();
        phi.cos() / (1.0 - self.e * self.e * s * s).sqrt()
    }

    fn t(&self, phi: f64) -> f64 {
        let s = phi.sin();
        let e = self.e;
        (PI / 4.0 - phi / 2.0).tan() / ((1.0 - e * s) / (1.0 + e * s)).powf(e / 2.0)
    }

    fn forward(&self, lon: f64, lat: f64) -> Pair {
        let phi = lat.to_radians();
        let rho = self.a * self.f_big * self.t(phi).powf(self.n);
        let theta = self.n * (lon.to_radians() - self.lon0);
        (self.fe + rho * theta.sin(), self.fn_ + self.rho0 - rho * theta.cos())
    }

    fn inverse(&self, east: f64, north: f64) -> Pair {
        let dx = east - self.fe;
        let dy = self.rho0 - (north - self.fn_);
        let rho = dx.hypot(dy).copysign(self.n);
        let t = (rho / (self.a * self.f_big)).powf(1.0 / self.n);
        let theta = dx.atan2(dy);
        let e = self.e;
        let mut phi = PI / 2.0 - 2.0 * t.atan();
        for _ in 0..8 {
            let s = phi.sin();
            phi = PI / 2.0 - 2.0 * (t * ((1.0 - e * s) / (1.0 + e * s)).powf(e / 2.0)).atan();
        }
        ((theta / self.n + self.lon0).to_degrees(), phi.to_degrees())
    }
}

/// Lambert Azimuthal Equal Area, ellipsoidal (EPSG 9820).
struct LambertAzimuthal {
    e: f64,
    e2: f64,
    lon0: f64,
    fe: f64,
    fn_: f64,
    qp: f64,
    b0: f64,
    rq: f64,
    d: f64,
}

impl LambertAzimuthal {
    fn new(ellipsoid: Ellipsoid, lat0: f64, lon0: f64, fe: f64, fnn: f64) -> Self {
        let (a, _f, e2) = ecc(ellipsoid);
        let mut la = LambertAzimuthal {
            e: e2.sqrt(),
            e2,
            lon0: lon0.to_radians(),
            fe,
            fn_: fnn,
            qp: 0.0,
            b0: 0.0,
            rq: 0.0,
            d: 0.0,
        };
        let p0 = lat0.to_radians();
        la.qp = la.q(PI / 2.0);
        la.b0 = (la.q(p0) / la.qp).asin();
        la.rq = a * (la.qp / 2.0).sqrt();
        let s0 = p0.sin();
        la.d = a * (p0.cos() / (1.0 - e2 * s0 * s0).sqrt()) / (la.rq * la.b0.cos());
        la
    }

    fn q(&self, phi: f64) -> f64 {
        let (e, e2) = (self.e, self.e2);
        let s = phi.sin();
        (1.0 - e2) * (s / (1.0 - e2 * s * s) - (1.0 / (2.0 * e)) * ((1.0 - e * s) / (1.0 + e * s)).ln())
    }

    fn forward(&self, lon: f64, lat: f64) -> Pair {
        let beta = (self.q(lat.to_radians()) / self.qp).clamp(-1.0, 1.0).asin();
        let dl = lon.to_radians() - self.lon0;
        let b0 = self.b0;
        let b = self.rq * (2.0 / (1.0 + b0.sin() * beta.sin() + b0.cos() * beta.cos() * dl.cos())).sqrt();
        let east = self.fe + b * self.d * beta.cos() * dl.sin();
        let north = self.fn_ + (b / self.d) * (b0.cos() * beta.sin() - b0.sin() * beta.cos() * dl.cos());
        (east, north)
    }

    fn inverse(&self, east: f64, north: f64) -> Pair {
        let x = east - self.fe;
        let y = north - self.fn_;
        let (d, b0) = (self.d, self.b0);
        let rho = (x / d).hypot(d * y);
        let (beta, lam);
        if rho < 1e-9 {
            beta = b0;
            lam = self.lon0;
        } else {
            let c = 2.0 * (rho / (2.0 * self.rq)).min(1.0).asin();
            beta = (c.cos() * b0.sin() + d * y * c.sin() * b0.cos() / rho).asin();
            lam = self.lon0 + (x * c.sin()).atan2(d * rho * b0.cos() * c.cos() - d * d * y * b0.sin() * c.sin());
        }
        let e2 = self.e2;
        let phi = beta
            + (e2 / 3.0 + 31.0 * e2.powi(2) / 180.0 + 517.0 * e2.powi(3) / 5040.0) * (2.0 * beta).sin()
            + (23.0 * e2.powi(2) / 360.0 + 251.0 * e2.powi(3) / 3780.0) * (4.0 * beta).sin()
            + (761.0 * e2.powi(3) / 45360.0) * (6.0 * beta).sin();
        (lam.to_degrees(), phi.to_degrees())
    }
}

fn geocentric(lon: f64, lat: f64, ellipsoid: Ellipsoid) -> (f64, f64, f64) {
    let (a, _f, e2) = ecc(ellipsoid);
    let (phi, lam) = (lat.to_radians(), lon.to_radians());
    let s = phi.sin();
    let nu = a / (1.0 - e2 * s * s).sqrt();
    (
        nu * phi.cos() * lam.cos(),
        nu * phi.cos() * lam.sin(),
        nu * (1.0 - e2) * s,
    )
}

fn geodetic(x: f64, y: f64, z: f64, ellipsoid: Ellipsoid) -> Pair {
    let (a, _f, e2) = ecc(ellipsoid);
    let p = x.hypot(y);
    let mut phi = z.atan2(p * (1.0 - e2));
    for _ in 0..6 {
        let s = phi.sin();
        let nu = a / (1.0 - e2 * s * s).sqrt();
        phi = (z + e2 * nu * s).atan2(p);
    }
    (y.atan2(x).to_degrees(), phi.to_degrees())
}

/// Seven parameter shift, position vector convention; `sign` -1 undoes it.
fn helmert(xyz: (f64, f64, f64), params: &[f64; 7], sign: f64) -> (f64, f64, f64) {
    let [tx, ty, tz, rx, ry, rz, ds] = params.map(|v| sign * v);
    let k = PI / 648000.0;
    let (rx, ry, rz) = (rx * k, ry * k, rz * k);
    let m = 1.0 + ds * 1e-6;
    let (x, y, z) = xyz;
    (
        tx + m * (x - rz * y + ry * z),
        ty + m * (rz * x + y - rx * z),
        tz + m * (-ry * x + rx * y + z),
    )
}

fn datum(ellipsoid: Ellipsoid, params: [f64; 7]) -> (Func, Func) {
    let to_wgs: Func = Arc::new(move |lon, lat| {
        let (x, y, z) = helmert(geocentric(lon, lat, ellipsoid), &params, 1.0);
        geodetic(x, y, z, WGS84)
    });
    let from_wgs: Func = Arc::new(move |lon, lat| {
        let (x, y, z) = helmert(geocentric(lon, lat, WGS84), &params, -1.0);
        geodetic(x, y, z, ellipsoid)
    });
    (to_wgs, from_wgs)
}

fn projected(epsg: i64, name: String, proj: TransverseMercator, datum: Option<(Func, Func)>) -> Crs {
    let proj = Arc::new(proj);
    projected_with(
        epsg,
        name,
        {
            let p = proj.clone();
            Arc::new(move |e, n| p.inverse(e, n))
        },
        Arc::new(move |lon, lat| proj.forward(lon, lat)),
        datum,
    )
}

fn projected_with(epsg: i64, name: String, inverse: Func, forward: Func, datum: Option<(Func, Func)>) -> Crs {
    match datum {
        None => Crs {
            epsg,
            name,
            geographic: false,
            to_lonlat: inverse,
            from_lonlat: forward,
        },
        Some((to_wgs, from_wgs)) => Crs {
            epsg,
            name,
            geographic: false,
            to_lonlat: Arc::new(move |e, n| {
                let (lon, lat) = inverse(e, n);
                to_wgs(lon, lat)
            }),
            from_lonlat: Arc::new(move |lon, lat| {
                let (lon, lat) = from_wgs(lon, lat);
                forward(lon, lat)
            }),
        },
    }
}

const MERC_R: f64 = 6378137.0;

fn merc_inverse(x: f64, y: f64) -> Pair {
    (
        (x / MERC_R).to_degrees(),
        (2.0 * (y / MERC_R).exp().atan() - PI / 2.0).to_degrees(),
    )
}

fn merc_forward(lon: f64, lat: f64) -> Pair {
    let lat = lat.clamp(-85.051129, 85.051129);
    (
        lon.to_radians() * MERC_R,
        (PI / 4.0 + lat.to_radians() / 2.0).tan().ln() * MERC_R,
    )
}

/// Swiss LV95 / LV03 by swisstopo's published approximate formulas.
fn swiss(e_off: f64, n_off: f64) -> (Func, Func) {
    let to: Func = Arc::new(move |east, north| {
        let y = (east - e_off - 600000.0) / 1e6;
        let x = (north - n_off - 200000.0) / 1e6;
        let lam = 2.6779094 + 4.728982 * y + 0.791484 * y * x + 0.1306 * y * x * x - 0.0436 * y.powi(3);
        let phi =
            16.9023892 + 3.238272 * x - 0.270978 * y * y - 0.002528 * x * x - 0.0447 * y * y * x - 0.0140 * x.powi(3);
        (lam * 100.0 / 36.0, phi * 100.0 / 36.0)
    });
    let from: Func = Arc::new(move |lon, lat| {
        let p = (lat * 3600.0 - 169028.66) / 10000.0;
        let lam = (lon * 3600.0 - 26782.5) / 10000.0;
        let east = 600072.37 + 211455.93 * lam - 10938.51 * lam * p - 0.36 * lam * p * p - 44.54 * lam.powi(3);
        let north = 200147.07 + 308807.95 * p + 3745.25 * lam * lam + 76.63 * p * p - 194.56 * lam * lam * p
            + 119.79 * p.powi(3);
        (east + e_off, north + n_off)
    });
    (to, from)
}

fn utm(zone: i64, south: bool, ellipsoid: Ellipsoid, false_e: f64) -> TransverseMercator {
    TransverseMercator::new(
        ellipsoid,
        zone as f64 * 6.0 - 183.0,
        0.9996,
        false_e,
        if south { 10000000.0 } else { 0.0 },
        0.0,
    )
}

fn identity(lon: f64, lat: f64) -> Pair {
    (lon, lat)
}

/// The coordinate system with EPSG code `epsg`, or `None` when unsupported.
pub fn get(epsg: i64) -> Option<Crs> {
    let code = epsg;
    if matches!(code, 4326 | 4258 | 4269 | 4283 | 4167 | 4617) {
        return Some(Crs {
            epsg: code,
            name: "Longitude and latitude".into(),
            geographic: true,
            to_lonlat: Arc::new(identity),
            from_lonlat: Arc::new(identity),
        });
    }
    if matches!(code, 3857 | 900913 | 102100 | 102113) {
        return Some(Crs {
            epsg: code,
            name: "Web Mercator".into(),
            geographic: false,
            to_lonlat: Arc::new(merc_inverse),
            from_lonlat: Arc::new(merc_forward),
        });
    }
    if (32601..=32660).contains(&code) {
        let zone = code - 32600;
        return Some(projected(
            code,
            format!("WGS 84 / UTM zone {zone}N"),
            utm(zone, false, WGS84, 500000.0),
            None,
        ));
    }
    if (32701..=32760).contains(&code) {
        let zone = code - 32700;
        return Some(projected(
            code,
            format!("WGS 84 / UTM zone {zone}S"),
            utm(zone, true, WGS84, 500000.0),
            None,
        ));
    }
    if (25828..=25838).contains(&code) {
        let zone = code - 25800;
        return Some(projected(
            code,
            format!("ETRS89 / UTM zone {zone}N"),
            utm(zone, false, GRS80, 500000.0),
            None,
        ));
    }
    if (26901..=26923).contains(&code) {
        let zone = code - 26900;
        return Some(projected(
            code,
            format!("NAD83 / UTM zone {zone}N"),
            utm(zone, false, GRS80, 500000.0),
            None,
        ));
    }
    let zone_prefixed = match code {
        5649 => Some(31),
        4647 => Some(32),
        5650 => Some(33),
        _ => None,
    };
    if let Some(zone) = zone_prefixed {
        return Some(projected(
            code,
            format!("ETRS89 / UTM zone {zone}N (zone in easting)"),
            utm(zone, false, GRS80, zone as f64 * 1e6 + 500000.0),
            None,
        ));
    }
    let gk = match code {
        31466 | 31492 => Some(2),
        31467 | 31493 => Some(3),
        31468 | 31494 => Some(4),
        31469 | 31495 => Some(5),
        _ => None,
    };
    if let Some(zone) = gk {
        let proj = TransverseMercator::new(BESSEL, zone as f64 * 3.0, 1.0, zone as f64 * 1e6 + 500000.0, 0.0, 0.0);
        return Some(projected(
            code,
            format!("DHDN / Gauss-Krüger zone {zone}"),
            proj,
            Some(datum(BESSEL, DHDN_TO_WGS84)),
        ));
    }
    match code {
        27700 => {
            let proj = TransverseMercator::new(AIRY, -2.0, 0.9996012717, 400000.0, -100000.0, 49.0);
            Some(projected(
                code,
                "OSGB36 / British National Grid".into(),
                proj,
                Some(datum(AIRY, OSGB36_TO_WGS84)),
            ))
        }
        2154 => {
            let proj = Arc::new(LambertConic::new(GRS80, 49.0, 44.0, 46.5, 3.0, 700000.0, 6600000.0));
            let p = proj.clone();
            Some(projected_with(
                code,
                "RGF93 / Lambert-93".into(),
                Arc::new(move |e, n| p.inverse(e, n)),
                Arc::new(move |lon, lat| proj.forward(lon, lat)),
                None,
            ))
        }
        3035 => {
            let proj = Arc::new(LambertAzimuthal::new(GRS80, 52.0, 10.0, 4321000.0, 3210000.0));
            let p = proj.clone();
            Some(projected_with(
                code,
                "ETRS89 / LAEA Europe".into(),
                Arc::new(move |e, n| p.inverse(e, n)),
                Arc::new(move |lon, lat| proj.forward(lon, lat)),
                None,
            ))
        }
        2056 => {
            let (to, from) = swiss(2000000.0, 1000000.0);
            Some(Crs {
                epsg: code,
                name: "CH1903+ / LV95".into(),
                geographic: false,
                to_lonlat: to,
                from_lonlat: from,
            })
        }
        21781 => {
            let (to, from) = swiss(0.0, 0.0);
            Some(Crs {
                epsg: code,
                name: "CH1903 / LV03".into(),
                geographic: false,
                to_lonlat: to,
                from_lonlat: from,
            })
        }
        _ => None,
    }
}

/// The list a refusal prints.
pub const SUPPORTED_HINT: &str = "WGS 84 (4326), ETRS89 (4258), Web Mercator (3857), UTM on WGS 84 \
(32601 to 32660, 32701 to 32760), ETRS89 / UTM (25828 to 25838, and \
4647, 5649, 5650 with the zone in the easting), NAD83 / UTM (26901 to \
26923), DHDN / Gauss-Krüger (31466 to 31469), British National Grid \
(27700), Lambert-93 (2154), LAEA Europe (3035), Swiss LV95 (2056) and \
LV03 (21781)";
