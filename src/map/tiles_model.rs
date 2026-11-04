//! The arithmetic and wording of the offline map dialog (`python/src/js/tiles.js`):
//! how many tiles an area and a zoom range come to, how big that is, the
//! bounds of what the map shows, and the line under each downloaded area.

use serde_json::Value;

use crate::map::regions::Bounds;

pub const AVG_TILE_KB: f64 = 15.0;
pub const ZOOM_SPAN: i64 = 4;
pub const FALLBACK_ZOOM_CAP: i64 = 19;
pub const BIG_JOB_TILES: i64 = 20_000;
pub const FALLBACK_MAX_TILES: i64 = 50_000;
const MERCATOR_LAT: f64 = 85.05112878;

fn lon_to_x(lon: f64, z: i64) -> i64 {
    (((lon + 180.0) / 360.0) * 2f64.powi(z as i32)).floor() as i64
}

fn lat_to_y(lat: f64, z: i64) -> i64 {
    let r = lat.to_radians();
    (((1.0 - (r.tan() + 1.0 / r.cos()).ln() / std::f64::consts::PI) / 2.0) * 2f64.powi(z as i32)).floor() as i64
}

/// `estimateTileCount`: the slippy map tiles of a box over a zoom range,
/// clamped to the grid as the backend does.
pub fn tile_count(b: &Bounds, minzoom: i64, maxzoom: i64) -> i64 {
    let (lo, hi) = (minzoom.min(maxzoom), minzoom.max(maxzoom));
    let mut count = 0;
    for z in lo..=hi {
        let last = (1i64 << z) - 1;
        let grid = |v: i64| v.clamp(0, last);
        let (x0, x1) = (lon_to_x(b.w, z), lon_to_x(b.e, z));
        let (y0, y1) = (lat_to_y(b.n, z), lat_to_y(b.s, z));
        let (xmin, xmax) = (grid(x0.min(x1)), grid(x0.max(x1)));
        let (ymin, ymax) = (grid(y0.min(y1)), grid(y0.max(y1)));
        count += (xmax - xmin + 1) * (ymax - ymin + 1);
    }
    count
}

/// `demCount`: tiles of one elevation job. It runs from zoom 0 (or as low as
/// the per job cap allows) up to the model's last level it has everywhere.
pub fn dem_count(b: &Bounds, maxzoom: i64, sparse_above: Option<i64>, lo: i64, hi: i64, cap: i64) -> i64 {
    let top = hi.min(maxzoom).min(sparse_above.unwrap_or(maxzoom));
    let mut low = 0;
    while low < lo.min(top) && tile_count(b, low, top) > cap {
        low += 1;
    }
    tile_count(b, low, top)
}

/// `fmtCount`.
pub fn fmt_count(n: i64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1e6)
    } else if n >= 1000 {
        format!("{:.1}k", n as f64 / 1e3)
    } else {
        n.to_string()
    }
}

/// `fmtSize`, for a size in kilobytes.
pub fn fmt_size(kb: f64) -> String {
    if kb >= 1e6 {
        format!("{:.1} GB", kb / 1e6)
    } else if kb >= 1e3 {
        format!("{:.0} MB", kb / 1e3)
    } else {
        format!("{} KB", kb.round())
    }
}

fn clamp_lat(lat: f64) -> f64 {
    lat.clamp(-MERCATOR_LAT, MERCATOR_LAT)
}

/// `oneWorld`: a view's bounds cut to one world copy and to Web Mercator's
/// latitudes.
pub fn one_world(w: f64, s: f64, e: f64, n: f64) -> Bounds {
    if e - w >= 360.0 {
        return Bounds {
            w: -180.0,
            s: clamp_lat(s),
            e: 180.0,
            n: clamp_lat(n),
        };
    }
    let shift = (((w + e) / 2.0 / 360.0).round()) * 360.0;
    Bounds {
        w: (w - shift).max(-180.0),
        s: clamp_lat(s),
        e: (e - shift).min(180.0),
        n: clamp_lat(n),
    }
}

/// The bounds of a view `width` by `height` px centred on a point at `zoom`.
pub fn view_bounds(lng: f64, lat: f64, zoom: f64, width: f64, height: f64) -> Bounds {
    let world = 256.0 * 2f64.powf(zoom);
    let px = |lon: f64| (lon + 180.0) / 360.0 * world;
    let py = |lat: f64| {
        let s = lat.to_radians().sin().clamp(-0.9999, 0.9999);
        (0.5 - ((1.0 + s) / (1.0 - s)).ln() / (4.0 * std::f64::consts::PI)) * world
    };
    let lon_of = |x: f64| x / world * 360.0 - 180.0;
    let lat_of = |y: f64| {
        let n = std::f64::consts::PI * (1.0 - 2.0 * y / world);
        n.sinh().atan().to_degrees()
    };
    let (cx, cy) = (px(lng), py(lat));
    one_world(
        lon_of(cx - width / 2.0),
        lat_of(cy + height / 2.0),
        lon_of(cx + width / 2.0),
        lat_of(cy - height / 2.0),
    )
}

/// `regionMeta`: source, zoom range, tiles or "downloading", a note when it
/// did not finish, and the date.
pub fn region_meta(r: &Value, source_label: &str) -> String {
    let mut parts = vec![source_label.to_string()];
    let z = |k: &str| {
        r.get(k)
            .and_then(Value::as_f64)
            .map_or("undefined".to_string(), |v| v.to_string())
    };
    parts.push(format!("z{} to {}", z("minzoom"), z("maxzoom")));
    let state = r.get("state").and_then(Value::as_str).unwrap_or("");
    parts.push(if state == "running" {
        "downloading\u{2026}".into()
    } else {
        format!(
            "{} tiles",
            fmt_count(r.get("tile_count").and_then(Value::as_i64).unwrap_or(0))
        )
    });
    if state == "cancelled" || state == "failed" {
        parts.push("incomplete".into());
    }
    if let Some(d) = r
        .get("created_at")
        .and_then(Value::as_str)
        .filter(|d| d.len() >= 10)
        .map(|d| d[..10].to_string())
    {
        parts.push(d);
    }
    parts
        .into_iter()
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join(" \u{b7} ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_small_box_over_two_zooms() {
        let b = Bounds {
            w: 11.6395,
            s: 48.0745,
            e: 11.6455,
            n: 48.0775,
        };
        assert_eq!(tile_count(&b, 14, 15), 2);
        assert_eq!(tile_count(&b, 15, 14), 2, "the order does not matter");
        let whole = Bounds {
            w: -180.0,
            s: -85.0,
            e: 180.0,
            n: 85.0,
        };
        assert_eq!(tile_count(&whole, 0, 1), 1 + 4);
    }

    #[test]
    fn elevation_starts_as_low_as_the_cap_allows() {
        let b = Bounds {
            w: 11.0,
            s: 48.0,
            e: 12.0,
            n: 49.0,
        };
        assert_eq!(dem_count(&b, 15, None, 12, 16, 1_000_000), tile_count(&b, 0, 15));
        let capped = dem_count(&b, 15, None, 12, 16, 500);
        assert!(capped <= tile_count(&b, 0, 15) && capped > 0);
        assert_eq!(dem_count(&b, 17, Some(12), 8, 16, 1_000_000), tile_count(&b, 0, 12));
    }

    #[test]
    fn counts_and_sizes_read_like_the_web() {
        assert_eq!(fmt_count(999), "999");
        assert_eq!(fmt_count(1500), "1.5k");
        assert_eq!(fmt_count(2_500_000), "2.5M");
        assert_eq!(fmt_size(300.0), "300 KB");
        assert_eq!(fmt_size(45_000.0), "45 MB");
        assert_eq!(fmt_size(2_400_000.0), "2.4 GB");
    }

    #[test]
    fn a_view_is_cut_to_one_world() {
        let b = one_world(-250.0, -90.0, 312.0, 90.0);
        assert_eq!((b.w, b.e), (-180.0, 180.0));
        assert!(b.n < 90.0 && b.s > -90.0);
        let c = one_world(170.0, 10.0, 190.0, 20.0);
        assert_eq!(
            (c.w, c.e),
            (-180.0, -170.0),
            "the centre on the date line goes to the western copy, as in the web"
        );
        let d = one_world(350.0, 10.0, 370.0, 20.0);
        assert_eq!((d.w, d.e), (-10.0, 10.0));
    }

    #[test]
    fn the_view_bounds_are_centred_on_the_view() {
        let b = view_bounds(11.64, 48.08, 15.0, 1000.0, 800.0);
        let (cx, cy) = b.centre();
        assert!((cx - 11.64).abs() < 1e-6 && (cy - 48.08).abs() < 1e-3);
        assert!((b.e - b.w - 0.0429).abs() < 0.001, "{}", b.e - b.w);
    }

    #[test]
    fn the_line_under_an_area() {
        let r = json!({"minzoom": 14, "maxzoom": 15, "tile_count": 1200, "state": "done", "created_at": "2026-10-29T11:57:23Z"});
        assert_eq!(
            region_meta(&r, "Esri"),
            "Esri \u{b7} z14 to 15 \u{b7} 1.2k tiles \u{b7} 2026-10-29"
        );
        let r = json!({"minzoom": 14, "maxzoom": 15, "state": "cancelled"});
        assert_eq!(
            region_meta(&r, "Esri"),
            "Esri \u{b7} z14 to 15 \u{b7} 0 tiles \u{b7} incomplete"
        );
        let r = json!({"minzoom": 1, "maxzoom": 2, "state": "running"});
        assert!(region_meta(&r, "").contains("downloading"));
    }
}
