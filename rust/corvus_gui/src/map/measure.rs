//! The distance tool of the Home map (`src/js/map-measure.js`): the rail's
//! ruler turns it on, a click puts down the first point, the line follows
//! the pointer, a second click fixes the end, and a click after that starts a
//! new line. The label on the line reads the great circle distance over the
//! ground and the heading from the first point to the second. Escape or a
//! right click clears the line; with none on the map they turn the tool off.

use crate::data::units::Units;

const EARTH_R_M: f64 = 6_371_008.8;

/// A longitude and latitude in degrees.
pub type LngLat = [f64; 2];

/// Great circle distance in metres.
pub fn distance_m(a: LngLat, b: LngLat) -> f64 {
    let (lat1, lat2) = (a[1].to_radians(), b[1].to_radians());
    let d_lat = lat2 - lat1;
    let d_lng = (b[0] - a[0]).to_radians();
    let h = (d_lat / 2.0).sin().powi(2) + lat1.cos() * lat2.cos() * (d_lng / 2.0).sin().powi(2);
    2.0 * EARTH_R_M * h.sqrt().min(1.0).asin()
}

/// Initial bearing from `a` to `b`, degrees clockwise from north, 0 to 360.
pub fn bearing_deg(a: LngLat, b: LngLat) -> f64 {
    let (lat1, lat2) = (a[1].to_radians(), b[1].to_radians());
    let d_lng = (b[0] - a[0]).to_radians();
    let y = d_lng.sin() * lat2.cos();
    let x = lat1.cos() * lat2.sin() - lat1.sin() * lat2.cos() * d_lng.cos();
    (y.atan2(x).to_degrees() + 360.0) % 360.0
}

/// The point halfway along the great circle from `a` to `b`.
pub fn midpoint(a: LngLat, b: LngLat) -> LngLat {
    let (lat1, lng1) = (a[1].to_radians(), a[0].to_radians());
    let lat2 = b[1].to_radians();
    let d_lng = (b[0] - a[0]).to_radians();
    let bx = lat2.cos() * d_lng.cos();
    let by = lat2.cos() * d_lng.sin();
    let lat = (lat1.sin() + lat2.sin()).atan2(((lat1.cos() + bx).powi(2) + by.powi(2)).sqrt());
    let lng = lng1 + by.atan2(lat1.cos() + bx);
    [(lng.to_degrees() + 540.0) % 360.0 - 180.0, lat.to_degrees()]
}

/// The label's text: a decimal below 100 m, whole numbers up to a kilometre,
/// then the distance unit (`formatDistance`).
pub fn format_distance(m: f64, units: &Units) -> String {
    if m < 1000.0 {
        units.format_length(m, usize::from(m < 100.0), false, false)
    } else {
        units.format_distance(m)
    }
}

/// The tool's state.
#[derive(Debug, Default)]
pub struct Measure {
    pub active: bool,
    points: Vec<LngLat>,
    cursor: Option<LngLat>,
    pub units: Units,
}

/// The line as it is drawn.
#[derive(Debug, PartialEq)]
pub struct Line {
    pub from: LngLat,
    pub to: LngLat,
    pub points: usize,
}

impl Measure {
    pub fn set_active(&mut self, on: bool) {
        self.active = on;
        if !on {
            self.clear();
        }
    }

    pub fn clear(&mut self) {
        self.points.clear();
        self.cursor = None;
    }

    /// A click: the second point when one is down, else a new first point.
    pub fn click(&mut self, p: LngLat) {
        if self.points.len() == 1 {
            self.points.push(p);
        } else {
            self.points = vec![p];
        }
        self.cursor = None;
    }

    /// The pointer moved: the end of the line while only one point is down.
    pub fn hover(&mut self, p: LngLat) {
        if self.points.len() == 1 {
            self.cursor = Some(p);
        }
    }

    /// Escape or a right click: give up the line, else leave the tool.
    pub fn back(&mut self) {
        if self.points.is_empty() {
            self.set_active(false);
        } else {
            self.clear();
        }
    }

    /// The line to draw, when there is one.
    pub fn line(&self) -> Option<Line> {
        let from = *self.points.first()?;
        let to = if self.points.len() == 2 {
            self.points[1]
        } else {
            self.cursor?
        };
        Some(Line {
            from,
            to,
            points: self.points.len(),
        })
    }

    /// The label on the line: the distance and, once it is more than zero,
    /// the heading.
    pub fn label(&self, line: &Line) -> (String, String) {
        let m = distance_m(line.from, line.to);
        let heading = if m > 0.0 {
            format!("{}\u{b0}", (bearing_deg(line.from, line.to).round() as i64) % 360)
        } else {
            String::new()
        };
        (format_distance(m, &self.units), heading)
    }

    pub fn points(&self) -> &[LngLat] {
        &self.points
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_degree_of_latitude_is_about_111_km() {
        let d = distance_m([11.0, 48.0], [11.0, 49.0]);
        assert!((d - 111_195.0).abs() < 50.0, "{d}");
        assert!((bearing_deg([11.0, 48.0], [11.0, 49.0]) - 0.0).abs() < 1e-9);
        assert!((bearing_deg([11.0, 48.0], [12.0, 48.0]) - 89.7).abs() < 0.6);
        let m = midpoint([11.0, 48.0], [11.0, 50.0]);
        assert!((m[1] - 49.0).abs() < 1e-6 && (m[0] - 11.0).abs() < 1e-9);
    }

    #[test]
    fn distances_read_in_the_units_chosen() {
        let u = Units::default();
        assert_eq!(format_distance(42.34, &u), "42.3 m");
        assert_eq!(format_distance(420.6, &u), "421 m");
        assert_eq!(format_distance(1234.0, &u), "1.23 km");
    }

    #[test]
    fn clicks_follow_the_web_tool() {
        let mut m = Measure::default();
        m.set_active(true);
        assert!(m.line().is_none());
        m.click([11.0, 48.0]);
        assert!(m.line().is_none(), "no end until the pointer moves");
        m.hover([11.001, 48.0]);
        assert_eq!(m.line().unwrap().points, 1);
        m.click([11.002, 48.0]);
        let line = m.line().unwrap();
        assert_eq!((line.points, line.to), (2, [11.002, 48.0]));
        m.hover([12.0, 48.0]);
        assert_eq!(m.line().unwrap().to, [11.002, 48.0], "a fixed end stays");
        m.click([11.5, 48.5]);
        assert_eq!(m.points().len(), 1, "a third click starts again");
        m.back();
        assert!(m.points().is_empty() && m.active);
        m.back();
        assert!(!m.active);
    }

    #[test]
    fn the_label_has_a_distance_and_a_heading() {
        let mut m = Measure::default();
        m.click([11.0, 48.0]);
        m.click([11.0, 48.001]);
        let l = m.line().unwrap();
        assert_eq!(m.label(&l), ("111 m".to_string(), "0\u{b0}".to_string()));
        let mut z = Measure::default();
        z.click([11.0, 48.0]);
        z.click([11.0, 48.0]);
        assert_eq!(z.label(&z.line().unwrap()).1, "");
    }
}
