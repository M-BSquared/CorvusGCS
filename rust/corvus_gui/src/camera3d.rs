//! The 3D map's camera and the ground it looks at, in plain `f64`
//! (`src/js/map.js` 3D mode, MapLibre's free camera).
//!
//! Everything is metres east, north and up around the view centre. The scale
//! of the web mercator plane is taken at the centre's latitude, which is
//! exact there and off by a fraction of a percent over tens of kilometres.
//! The camera looks at the centre from the distance that gives the metres per
//! pixel of the flat map at the same zoom, so a flat view and a 3D view at
//! pitch 0 agree.

/// The earth radius of web mercator.
pub const R: f64 = 6_378_137.0;
/// MapLibre's vertical field of view.
pub const FOV_DEG: f64 = 36.87;
/// `THREE_D_PITCH`: where the camera goes when 3D is switched on.
pub const THREE_D_PITCH: f64 = 60.0;
pub const MAX_PITCH: f64 = 85.0;

pub type V3 = [f64; 3];

fn dot(a: V3, b: V3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: V3, b: V3) -> V3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

/// Web mercator metres of a longitude and latitude in degrees.
pub fn mercator(lng: f64, lat: f64) -> (f64, f64) {
    let x = lng.to_radians() * R;
    let y = (std::f64::consts::FRAC_PI_4 + lat.to_radians() / 2.0).tan().ln() * R;
    (x, y)
}

/// Longitude and latitude in degrees of web mercator metres.
pub fn unmercator(x: f64, y: f64) -> (f64, f64) {
    let lng = (x / R).to_degrees();
    let lat = (2.0 * (y / R).exp().atan() - std::f64::consts::FRAC_PI_2).to_degrees();
    (lng, lat)
}

/// True metres per pixel at `lat` and `zoom` on a world of 256 px tiles.
pub fn metres_per_pixel(lat: f64, zoom: f64) -> f64 {
    2.0 * std::f64::consts::PI * R * lat.to_radians().cos() / (256.0 * 2f64.powf(zoom))
}

/// A point on the screen and how far it is along the view direction.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Screen {
    pub x: f64,
    pub y: f64,
    pub depth: f64,
}

/// Where the camera looks from and at.
#[derive(Debug, Clone, Copy)]
pub struct Camera {
    pub lng: f64,
    pub lat: f64,
    pub zoom: f64,
    pub pitch: f64,
    pub bearing: f64,
    /// The size of the view in pixels.
    pub size: (f64, f64),
    /// The height above sea level of the point looked at; `up` is measured
    /// from it.
    pub ground: f64,
}

impl Camera {
    fn scale(&self) -> f64 {
        self.lat.to_radians().cos()
    }

    /// Focal length in pixels.
    pub fn focal(&self) -> f64 {
        (self.size.1 / 2.0) / (FOV_DEG.to_radians() / 2.0).tan()
    }

    /// Metres between the camera and the point it looks at.
    pub fn distance(&self) -> f64 {
        self.focal() * metres_per_pixel(self.lat, self.zoom)
    }

    /// The camera's position and its right, up and forward unit vectors.
    fn basis(&self) -> (V3, V3, V3, V3) {
        let (p, b) = (self.pitch.to_radians(), self.bearing.to_radians());
        let fh = [b.sin(), b.cos(), 0.0];
        let f = [p.sin() * fh[0], p.sin() * fh[1], -p.cos()];
        let r = [b.cos(), -b.sin(), 0.0];
        let u = cross(r, f);
        let d = self.distance();
        let pos = [-f[0] * d, -f[1] * d, -f[2] * d];
        (pos, r, u, f)
    }

    /// Where the camera is, in the same metres.
    pub fn eye(&self) -> V3 {
        self.basis().0
    }

    /// Metres east, north and up of a position, up from `ground`.
    pub fn local_of(&self, lng: f64, lat: f64, height: f64) -> V3 {
        let (cx, cy) = mercator(self.lng, self.lat);
        let (x, y) = mercator(lng, lat);
        let k = self.scale();
        [(x - cx) * k, (y - cy) * k, height - self.ground]
    }

    /// Longitude and latitude of metres east and north of the centre.
    pub fn geo_of(&self, east: f64, north: f64) -> (f64, f64) {
        let (cx, cy) = mercator(self.lng, self.lat);
        let k = self.scale();
        unmercator(cx + east / k, cy + north / k)
    }

    /// The pixel of a local point, `None` behind the camera.
    pub fn project(&self, p: V3) -> Option<Screen> {
        let (pos, r, u, f) = self.basis();
        let v = sub(p, pos);
        let depth = dot(v, f);
        if depth < self.distance() * 0.02 {
            return None;
        }
        let k = self.focal() / depth;
        Some(Screen {
            x: self.size.0 / 2.0 + dot(v, r) * k,
            y: self.size.1 / 2.0 - dot(v, u) * k,
            depth,
        })
    }

    /// The local east and north where the ray through pixel `(x, y)` meets
    /// the plane `up` metres above the centre's ground; `None` when the ray
    /// does not go down to it (above the horizon).
    pub fn ground_at(&self, x: f64, y: f64, up: f64) -> Option<(f64, f64)> {
        let (pos, r, u, f) = self.basis();
        let k = 1.0 / self.focal();
        let dx = (x - self.size.0 / 2.0) * k;
        let dy = -(y - self.size.1 / 2.0) * k;
        let ray = [
            f[0] + r[0] * dx + u[0] * dy,
            f[1] + r[1] * dx + u[1] * dy,
            f[2] + r[2] * dx + u[2] * dy,
        ];
        if ray[2] >= -1e-9 {
            return None;
        }
        let t = (up - pos[2]) / ray[2];
        (t > 0.0).then(|| (pos[0] + ray[0] * t, pos[1] + ray[1] * t))
    }

    /// Moves the centre so the ground that was under pixel `from` is under
    /// pixel `to`: a drag on the map.
    pub fn pan(&mut self, from: (f64, f64), to: (f64, f64)) {
        if let (Some(a), Some(b)) = (self.ground_at(from.0, from.1, 0.0), self.ground_at(to.0, to.1, 0.0)) {
            let (lng, lat) = self.geo_of(a.0 - b.0, a.1 - b.1);
            self.lng = lng;
            self.lat = lat;
        }
    }

    /// Turns by `dx` px and tilts by `dy` px of a right drag.
    pub fn turn(&mut self, dx: f64, dy: f64) {
        self.bearing = (self.bearing + dx * 0.4).rem_euclid(360.0);
        self.pitch = (self.pitch - dy * 0.25).clamp(0.0, MAX_PITCH);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cam(pitch: f64, bearing: f64) -> Camera {
        Camera {
            lng: 11.64,
            lat: 48.08,
            zoom: 16.0,
            pitch,
            bearing,
            size: (800.0, 600.0),
            ground: 500.0,
        }
    }

    #[test]
    fn the_mercator_round_trip() {
        let (x, y) = mercator(11.64, 48.08);
        let (lng, lat) = unmercator(x, y);
        assert!((lng - 11.64).abs() < 1e-9 && (lat - 48.08).abs() < 1e-9);
    }

    #[test]
    fn the_centre_is_the_middle_of_the_view() {
        for pitch in [0.0, 60.0] {
            let s = cam(pitch, 0.0).project([0.0, 0.0, 0.0]).unwrap();
            assert!((s.x - 400.0).abs() < 1e-6 && (s.y - 300.0).abs() < 1e-6);
        }
    }

    #[test]
    fn a_flat_view_has_the_metres_per_pixel_of_the_flat_map() {
        let c = cam(0.0, 0.0);
        let mpp = metres_per_pixel(c.lat, c.zoom);
        let s = c.project([100.0, 0.0, 0.0]).unwrap();
        assert!((s.x - 400.0 - 100.0 / mpp).abs() < 1e-6);
        let n = c.project([0.0, 100.0, 0.0]).unwrap();
        assert!(n.y < 300.0, "north is up");
    }

    #[test]
    fn the_bearing_turns_what_is_up() {
        let c = cam(0.0, 90.0);
        let e = c.project([100.0, 0.0, 0.0]).unwrap();
        assert!(e.y < 300.0 && (e.x - 400.0).abs() < 1e-6, "east is up facing east");
    }

    #[test]
    fn a_tilted_view_shows_the_far_ground_higher_and_smaller() {
        let c = cam(60.0, 0.0);
        let near = c.project([0.0, -100.0, 0.0]).unwrap();
        let far = c.project([0.0, 100.0, 0.0]).unwrap();
        assert!(far.y < near.y);
        let a = c.project([-50.0, 100.0, 0.0]).unwrap();
        let b = c.project([50.0, 100.0, 0.0]).unwrap();
        let a2 = c.project([-50.0, -100.0, 0.0]).unwrap();
        let b2 = c.project([50.0, -100.0, 0.0]).unwrap();
        assert!(b.x - a.x < b2.x - a2.x);
    }

    #[test]
    fn height_lifts_a_point() {
        let c = cam(60.0, 0.0);
        let low = c.project([0.0, 0.0, 0.0]).unwrap();
        let high = c.project([0.0, 0.0, 100.0]).unwrap();
        assert!(high.y < low.y);
    }

    #[test]
    fn a_pixel_goes_back_to_the_ground() {
        let c = cam(60.0, 30.0);
        let p = [120.0, -40.0, 0.0];
        let s = c.project(p).unwrap();
        let (e, n) = c.ground_at(s.x, s.y, 0.0).unwrap();
        assert!((e - p[0]).abs() < 1e-6 && (n - p[1]).abs() < 1e-6);
        assert!(
            cam(80.0, 0.0).ground_at(400.0, 0.0, 0.0).is_none(),
            "the sky has no ground"
        );
    }

    #[test]
    fn a_drag_keeps_the_ground_under_the_pointer() {
        let mut c = cam(60.0, 20.0);
        let before = c.ground_at(500.0, 400.0, 0.0).map(|(e, n)| c.geo_of(e, n)).unwrap();
        c.pan((500.0, 400.0), (450.0, 380.0));
        let (e, n) = c.ground_at(450.0, 380.0, 0.0).unwrap();
        let after = c.geo_of(e, n);
        assert!((before.0 - after.0).abs() < 1e-7 && (before.1 - after.1).abs() < 1e-7);
    }

    #[test]
    fn tilting_stays_between_flat_and_the_limit() {
        let mut c = cam(60.0, 0.0);
        c.turn(0.0, -1000.0);
        assert_eq!(c.pitch, MAX_PITCH);
        c.turn(0.0, 1000.0);
        assert_eq!(c.pitch, 0.0);
    }
}
