//! The flown track as a function of time: a path, a speed, and a model that
//! says where the aircraft is, how it lies in the air and how far its battery
//! is down. Paths are made around a home point in a flat local frame, good to
//! centimetres over a field. It is no flight dynamics: the attitude follows
//! from the path.

use std::f64::consts::PI;

use serde_json::Value;

const G: f64 = 9.80665;
/// Where the pictures' hop across the field begins and ends, `(lat, lon)`.
pub const HOP_START: (f64, f64) = (48.075718, 11.642051);
pub const HOP_END: (f64, f64) = (48.076507, 11.644507);
/// How far the hop sags right of the straight line, as a share of its length.
const HOP_SAG: f64 = 0.084;

/// Metres per degree of latitude and of longitude.
fn metres(lat: f64) -> (f64, f64) {
    let r = lat.to_radians();
    (
        111132.92 - 559.82 * (2.0 * r).cos() + 1.175 * (4.0 * r).cos(),
        111412.84 * r.cos() - 93.5 * (3.0 * r).cos(),
    )
}

/// `home` moved `north` and `east` metres.
pub fn offset(home: (f64, f64), north: f64, east: f64) -> (f64, f64) {
    let (mn, me) = metres(home.0);
    (home.0 + north / mn, home.1 + east / me)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub lat: f64,
    pub lon: f64,
    pub alt: f64,
}

#[derive(Debug, Clone)]
pub struct Path {
    pub parked: bool,
    pub points: Vec<Point>,
    pub speed: f64,
    pub looped: bool,
}

fn ring(home: (f64, f64), n: usize, alt: f64, at: impl Fn(f64) -> (f64, f64)) -> Vec<Point> {
    (0..n)
        .map(|i| {
            let (north, east) = at(2.0 * PI * i as f64 / n as f64);
            let (lat, lon) = offset(home, north, east);
            Point { lat, lon, alt }
        })
        .collect()
}

/// The path `kind` around `home`; `opts` are its numbers by name.
pub fn path(kind: &str, home: (f64, f64), opts: &Value) -> Result<Path, String> {
    let n = |k: &str, d: f64| opts[k].as_f64().unwrap_or(d);
    let end = opts["end"]
        .as_array()
        .and_then(|a| Some((a.first()?.as_f64()?, a.get(1)?.as_f64()?)));
    let end = end.unwrap_or(HOP_END);
    let (mn, me) = metres(home.0);
    let (dn, de) = ((end.0 - home.0) * mn, (end.1 - home.1) * me);
    let length = dn.hypot(de).max(1e-9);
    let made = |points, speed, looped| Path {
        parked: kind == "parked",
        points,
        speed,
        looped,
    };
    Ok(match kind {
        "parked" => made(ring(home, 6, 0.0, |a| (0.02 * a.sin(), 0.02 * a.cos())), 0.002, true),
        "hover" => {
            let drift = n("drift", 3.0);
            made(
                ring(home, 12, n("alt", 30.0), |a| (drift * a.sin(), drift * a.cos())),
                1.2,
                true,
            )
        }
        "orbit" => {
            let r = n("radius", 120.0);
            made(
                ring(home, 48, n("alt", 60.0), |a| (r * a.cos(), -r * a.sin())),
                9.0,
                true,
            )
        }
        "figure_eight" => {
            let size = n("size", 150.0);
            let at = |t: f64| {
                let d = 1.0 + t.sin() * t.sin();
                (size * t.cos() / d, size * t.sin() * t.cos() / d)
            };
            made(ring(home, 720, n("alt", 50.0), at), 10.0, true)
        }
        "survey" => {
            let (w, h, alt) = (n("width", 300.0), n("height", 200.0), n("alt", 80.0));
            let legs = ((h / n("spacing", 45.0)) as usize + 1).max(2);
            let rot = n("heading", 0.0).to_radians();
            let mut points = Vec::new();
            for i in 0..legs {
                let y = -h / 2.0 + i as f64 * (h / (legs - 1) as f64);
                let xs = if i % 2 == 0 {
                    [-w / 2.0, w / 2.0]
                } else {
                    [w / 2.0, -w / 2.0]
                };
                for x in xs {
                    let (lat, lon) = offset(home, x * rot.sin() + y * rot.cos(), x * rot.cos() - y * rot.sin());
                    points.push(Point { lat, lon, alt });
                }
            }
            made(points, 12.0, true)
        }
        // From home to the end along a gentle curve, flown once and held there.
        "swoop" => {
            let bulge = opts["bulge"].as_f64().unwrap_or(-HOP_SAG * length);
            let (cn, ce) = (
                dn / 2.0 + 2.0 * bulge * de / length,
                de / 2.0 - 2.0 * bulge * dn / length,
            );
            let count = n("points", 24.0).max(2.0) as usize;
            let points = (0..=count)
                .map(|i| {
                    let u = i as f64 / count as f64;
                    let (lat, lon) = offset(
                        home,
                        2.0 * (1.0 - u) * u * cn + u * u * dn,
                        2.0 * (1.0 - u) * u * ce + u * u * de,
                    );
                    Point {
                        lat,
                        lon,
                        alt: n("alt", 40.0),
                    }
                })
                .collect();
            made(points, n("speed", 6.0), false)
        }
        // A rover's snaking lines from home to the end, at no height.
        "slalom" => {
            let amplitude = n("amplitude", 6.0);
            let waves = (length / n("wavelength", 26.0)).round().max(1.0);
            let count = waves as usize * 24;
            let (an, ae) = (dn / length, de / length);
            let points = (0..=count)
                .map(|i| {
                    let u = i as f64 / count as f64;
                    let side = amplitude * (2.0 * PI * waves * u).sin();
                    let (lat, lon) = offset(home, u * dn + side * ae, u * de - side * an);
                    Point { lat, lon, alt: 0.0 }
                })
                .collect();
            made(points, n("speed", 2.5), false)
        }
        other => return Err(format!("no path called {other}")),
    })
}

/// One instant of a flight.
#[derive(Debug, Clone, Copy, Default)]
pub struct Fix {
    pub lat: f64,
    pub lon: f64,
    pub alt_agl: f64,
    /// Degrees.
    pub heading: f64,
    pub ground_speed: f64,
    pub climb: f64,
    /// Radians.
    pub roll: f64,
    pub pitch: f64,
    pub vx: f64,
    pub vy: f64,
    pub landed: bool,
    pub takeoff: bool,
}

/// `path` flown from `home`.
#[derive(Debug, Clone)]
pub struct Model {
    pub path: Path,
    pub home: (f64, f64),
    /// Seconds spent climbing before the path begins.
    pub takeoff: f64,
    legs: Vec<(Point, Point, f64)>,
    length: f64,
}

fn ease(x: f64) -> f64 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

fn turn(a: f64, b: f64) -> f64 {
    (a - b + 180.0).rem_euclid(360.0) - 180.0
}

impl Model {
    pub fn new(path: Path, home: (f64, f64), takeoff: f64) -> Model {
        let mut points = path.points.clone();
        if path.looped {
            points.push(points[0]);
        }
        let (mn, me) = metres(home.0);
        let legs: Vec<_> = points
            .windows(2)
            .map(|w| {
                let d = ((w[1].lat - w[0].lat) * mn).hypot((w[1].lon - w[0].lon) * me);
                (w[0], w[1], d)
            })
            .collect();
        let length = legs.iter().map(|l| l.2).sum();
        Model {
            path,
            home,
            takeoff,
            legs,
            length,
        }
    }

    fn bearing(&self, a: (f64, f64), b: (f64, f64)) -> f64 {
        let (mn, me) = metres(self.home.0);
        ((b.1 - a.1) * me)
            .atan2((b.0 - a.0) * mn)
            .to_degrees()
            .rem_euclid(360.0)
    }

    fn leg(&self, dist: f64) -> (Point, Point, f64) {
        let mut left = dist.rem_euclid(self.length.max(1e-6));
        for (a, b, l) in &self.legs {
            if left <= *l || *l <= 0.0 {
                return (*a, *b, if *l > 0.0 { left / l } else { 0.0 });
            }
            left -= l;
        }
        let (a, b, _) = self.legs[self.legs.len() - 1];
        (a, b, 1.0)
    }

    fn at(&self, dist: f64) -> (f64, f64, f64) {
        let dist = if self.path.looped {
            dist
        } else {
            dist.clamp(0.0, self.length * (1.0 - 1e-9))
        };
        let (a, b, k) = self.leg(dist);
        (
            a.lat + (b.lat - a.lat) * k,
            a.lon + (b.lon - a.lon) * k,
            a.alt + (b.alt - a.alt) * k,
        )
    }

    /// The heading at `dist`, from a chord across it: a leg's own bearing steps at every corner.
    fn tangent(&self, dist: f64) -> f64 {
        let (a, b) = (self.at(dist - 1.5), self.at(dist + 1.5));
        if (a.0 - b.0).abs() + (a.1 - b.1).abs() < 1e-12 {
            let (a, b, _) = self.leg(dist);
            return self.bearing((a.lat, a.lon), (b.lat, b.lon));
        }
        self.bearing((a.0, a.1), (b.0, b.1))
    }

    /// The aircraft `t` seconds into the flight.
    pub fn sample(&self, t: f64) -> Fix {
        let first = self.path.points[0];
        if t < self.takeoff {
            let k = (t / self.takeoff.max(0.1)).max(0.0);
            let second = self.path.points[1];
            return Fix {
                lat: self.home.0,
                lon: self.home.1,
                alt_agl: first.alt * ease(k),
                heading: self.bearing((first.lat, first.lon), (second.lat, second.lon)),
                ground_speed: 0.4 * k,
                climb: first.alt / self.takeoff * (1.0 - (2.0 * k - 1.0).abs()) * 1.5,
                pitch: (-1.5_f64).to_radians(),
                landed: t <= 0.5,
                takeoff: t > 0.5,
                ..Fix::default()
            };
        }
        if self.path.parked {
            return Fix {
                lat: first.lat,
                lon: first.lon,
                heading: 62.0,
                roll: 0.004,
                pitch: -0.006,
                landed: true,
                ..Fix::default()
            };
        }
        let mut flown = (t - self.takeoff) * self.path.speed;
        let arrived = !self.path.looped && flown >= self.length;
        if arrived {
            flown = self.length * (1.0 - 1e-9);
        }
        let speed = if arrived { 0.0 } else { self.path.speed };
        let (lat, lon, alt) = self.at(flown);
        let heading = self.tangent(flown);
        let rate = turn(self.tangent(flown + speed * 0.5), self.tangent(flown - speed * 0.5));
        let r = heading.to_radians();
        Fix {
            lat,
            lon,
            alt_agl: alt,
            heading,
            ground_speed: speed,
            roll: (rate.to_radians() * speed / G).atan().clamp(-0.6, 0.6),
            pitch: -(2.0 + speed * 0.6).to_radians(),
            vx: speed * r.cos(),
            vy: speed * r.sin(),
            ..Fix::default()
        }
    }

    /// Volts, amperes and percent left after `t` seconds, of a 6S 16 Ah pack.
    pub fn battery(&self, t: f64) -> (f64, f64, i64) {
        let t = t.max(0.0);
        let (drawn, amps) = if self.path.parked {
            (0.9 * t / 3600.0, 0.9)
        } else {
            (38.0 * t / 3600.0, 38.0 * if t < self.takeoff { 0.35 } else { 1.0 })
        };
        let used = (drawn / 16.0).min(1.0);
        let cell = 4.15 - 0.55 * used - 0.25 * used.powi(6);
        (cell * 6.0, amps, ((1.0 - used) * 100.0).round() as i64)
    }

    /// West, south, east and north of the path and its home.
    pub fn bounds(&self) -> [f64; 4] {
        let lats = self.path.points.iter().map(|p| p.lat).chain([self.home.0]);
        let lons = self.path.points.iter().map(|p| p.lon).chain([self.home.1]);
        [
            lons.clone().fold(f64::MAX, f64::min),
            lats.clone().fold(f64::MAX, f64::min),
            lons.fold(f64::MIN, f64::max),
            lats.fold(f64::MIN, f64::max),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_hop_lifts_off_flies_its_curve_once_and_holds_at_its_end() {
        let m = Model::new(path("swoop", HOP_START, &json!({})).unwrap(), HOP_START, 8.0);
        assert!(m.sample(0.0).landed && m.sample(4.0).takeoff);
        assert!((m.sample(4.0).alt_agl - 20.0).abs() < 0.1);
        let mid = m.sample(20.0);
        assert!(mid.ground_speed > 5.9 && mid.alt_agl == 40.0);
        let end = m.sample(300.0);
        assert_eq!(end.ground_speed, 0.0);
        assert!((end.lat - HOP_END.0).abs() < 1e-5 && (end.lon - HOP_END.1).abs() < 1e-5);
        assert!(
            (40.0..52.0).contains(&end.heading),
            "it arrives pointing north east: {}",
            end.heading
        );
        let (v, _, left) = m.battery(60.0);
        assert!((24.0..25.0).contains(&v) && left == 96, "{v} {left}");
    }

    #[test]
    fn every_path_is_made_and_flown() {
        for kind in ["parked", "hover", "orbit", "figure_eight", "survey", "swoop", "slalom"] {
            let m = Model::new(path(kind, HOP_START, &json!({})).unwrap(), HOP_START, 1.0);
            for t in 0..200 {
                let f = m.sample(f64::from(t));
                assert!(
                    f.lat.is_finite() && f.lon.is_finite() && f.heading.is_finite() && f.roll.abs() <= 0.6,
                    "{kind}"
                );
            }
        }
        assert!(path("loop", HOP_START, &json!({})).is_err());
    }
}
