//! The downloaded areas the Home map can outline (`mergeRegions`,
//! `setRegions` in src/js/map.js, `.region-label` in map.css): the named
//! regions of `GET /api/tiles/regions`, those over the same ground merged
//! into one box with one label.

use serde_json::Value;

/// A box in degrees: west, south, east, north.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bounds {
    pub w: f64,
    pub s: f64,
    pub e: f64,
    pub n: f64,
}

impl Bounds {
    fn area(&self) -> f64 {
        (self.e - self.w).max(0.0) * (self.n - self.s).max(0.0)
    }

    /// `sameGround`: the overlap is at least `share` of the larger box.
    pub fn same_ground(&self, other: &Bounds, share: f64) -> bool {
        let overlap = Bounds {
            w: self.w.max(other.w),
            e: self.e.min(other.e),
            s: self.s.max(other.s),
            n: self.n.min(other.n),
        }
        .area();
        let larger = self.area().max(other.area());
        if larger == 0.0 {
            return self == other;
        }
        overlap / larger >= share
    }

    pub fn centre(&self) -> (f64, f64) {
        ((self.w + self.e) / 2.0, (self.s + self.n) / 2.0)
    }
}

/// One area as the backend lists it.
#[derive(Debug, Clone, PartialEq)]
pub struct Region {
    pub name: String,
    pub source: String,
    pub bounds: Bounds,
    pub minzoom: Option<f64>,
    pub maxzoom: Option<f64>,
    pub running: bool,
}

/// One box to draw.
#[derive(Debug, Clone, PartialEq)]
pub struct Merged {
    pub names: Vec<String>,
    pub bounds: Bounds,
    pub minzoom: Option<f64>,
    pub maxzoom: Option<f64>,
    pub running: bool,
}

impl Merged {
    pub fn name(&self) -> String {
        self.names.join(", ")
    }

    /// The label's second line.
    pub fn meta(&self) -> String {
        if self.running {
            "downloading\u{2026}".into()
        } else {
            format!(
                "z{} to {}",
                self.minzoom.map_or("undefined".into(), |z| z.to_string()),
                self.maxzoom.map_or("undefined".into(), |z| z.to_string())
            )
        }
    }
}

/// The regions of an `/api/tiles/regions` answer; entries without finite
/// bounds are dropped.
pub fn read(doc: &Value) -> Vec<Region> {
    doc.get("regions")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|r| {
                    let b = r.get("bounds")?;
                    let n = |k: &str| b.get(k).and_then(Value::as_f64).filter(|v| v.is_finite());
                    Some(Region {
                        name: r
                            .get("name")
                            .and_then(Value::as_str)
                            .filter(|s| !s.is_empty())
                            .unwrap_or("Region")
                            .to_string(),
                        source: r.get("source").and_then(Value::as_str).unwrap_or("").to_string(),
                        bounds: Bounds {
                            w: n("w")?,
                            s: n("s")?,
                            e: n("e")?,
                            n: n("n")?,
                        },
                        minzoom: r.get("minzoom").and_then(Value::as_f64),
                        maxzoom: r.get("maxzoom").and_then(Value::as_f64),
                        running: r.get("state").and_then(Value::as_str) == Some("running"),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// `mergeRegions`: areas over nearly the same ground (90 percent) become one
/// box, the names once each, the zoom range of all, "downloading" while any
/// still is. Regions of the sources `skip` names (elevation) are left out.
pub fn merge(list: &[Region], skip: &dyn Fn(&str) -> bool) -> Vec<Merged> {
    let mut groups: Vec<Merged> = Vec::new();
    for r in list.iter().filter(|r| !skip(&r.source)) {
        match groups.iter_mut().find(|g| g.bounds.same_ground(&r.bounds, 0.9)) {
            None => groups.push(Merged {
                names: vec![r.name.clone()],
                bounds: r.bounds,
                minzoom: r.minzoom,
                maxzoom: r.maxzoom,
                running: r.running,
            }),
            Some(g) => {
                if !g.names.contains(&r.name) {
                    g.names.push(r.name.clone());
                }
                g.bounds = Bounds {
                    w: g.bounds.w.min(r.bounds.w),
                    s: g.bounds.s.min(r.bounds.s),
                    e: g.bounds.e.max(r.bounds.e),
                    n: g.bounds.n.max(r.bounds.n),
                };
                g.minzoom = match (g.minzoom, r.minzoom) {
                    (Some(a), Some(b)) => Some(a.min(b)),
                    (a, b) => a.or(b),
                };
                g.maxzoom = match (g.maxzoom, r.maxzoom) {
                    (Some(a), Some(b)) => Some(a.max(b)),
                    (a, b) => a.or(b),
                };
                g.running |= r.running;
            }
        }
    }
    groups
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn region(name: &str, source: &str, w: f64, e: f64, z: (f64, f64), running: bool) -> Region {
        Region {
            name: name.into(),
            source: source.into(),
            bounds: Bounds { w, s: 48.0, e, n: 48.1 },
            minzoom: Some(z.0),
            maxzoom: Some(z.1),
            running,
        }
    }

    #[test]
    fn the_same_ground_downloaded_twice_is_one_box() {
        let list = [
            region("Field", "satellite", 11.0, 11.1, (8.0, 15.0), false),
            region("Field 2", "streets", 11.001, 11.1, (10.0, 18.0), true),
            region("Elsewhere", "satellite", 12.0, 12.1, (8.0, 12.0), false),
        ];
        let m = merge(&list, &|_| false);
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].name(), "Field, Field 2");
        assert_eq!((m[0].minzoom, m[0].maxzoom), (Some(8.0), Some(18.0)));
        assert!(m[0].running);
        assert_eq!(m[0].meta(), "downloading\u{2026}");
        assert_eq!(m[1].meta(), "z8 to 12");
    }

    #[test]
    fn elevation_is_not_drawn_twice() {
        let list = [
            region("Field", "satellite", 11.0, 11.1, (8.0, 15.0), false),
            region("Field", "terrain", 11.0, 11.1, (8.0, 12.0), false),
        ];
        let m = merge(&list, &|s| s == "terrain");
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].maxzoom, Some(15.0));
    }

    #[test]
    fn the_answer_is_read_and_bad_bounds_are_dropped() {
        let doc = json!({"regions": [
            {"name": "A", "source": "osm", "bounds": {"w": 1, "s": 2, "e": 3, "n": 4}, "minzoom": 5, "maxzoom": 9, "state": "done"},
            {"name": "B", "bounds": {"w": 1}},
            {"source": "osm", "bounds": {"w": 1, "s": 2, "e": 3, "n": 4}},
        ]});
        let r = read(&doc);
        assert_eq!(r.len(), 2);
        assert_eq!(r[1].name, "Region");
        assert_eq!(read(&json!({})).len(), 0);
    }
}
