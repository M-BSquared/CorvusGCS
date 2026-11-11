//! The checks the tile routes make on what a client sent (`http_input.py`):
//! bounds, zooms, region names and the query values of the search.

use serde_json::Value;

use super::geocode::Near;

pub const MAX_REGION_NAME: usize = 60;

/// Normalises an operator supplied region name: whitespace collapsed, cut at
/// 60 characters. Anything that is not text is empty, so the caller falls back
/// to a generated name.
pub fn clean_region_name(raw: Option<&Value>) -> String {
    let Some(text) = raw.and_then(Value::as_str) else {
        return String::new();
    };
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(MAX_REGION_NAME)
        .collect()
}

/// The name of an unnamed download: its centre coordinates.
pub fn default_region_name(bounds: (f64, f64, f64, f64)) -> String {
    let (w, s, e, n) = bounds;
    let lat = (s + n) / 2.0;
    let lon = (w + e) / 2.0;
    format!(
        "{:.3}\u{b0}{} {:.3}\u{b0}{}",
        lat.abs(),
        if lat >= 0.0 { 'N' } else { 'S' },
        lon.abs(),
        if lon >= 0.0 { 'E' } else { 'W' }
    )
}

/// `int(str(raw).strip())` or the fallback.
pub fn int_or(raw: Option<&str>, fallback: i64) -> i64 {
    raw.and_then(super::util::py_int).unwrap_or(fallback)
}

/// `"<lon>,<lat>"` from a query string, or None. Only ever a ranking hint, so
/// anything unparseable is dropped.
pub fn parse_near(raw: &str) -> Option<Near> {
    let parts: Vec<&str> = raw.split(',').collect();
    if parts.len() != 2 {
        return None;
    }
    let lon = super::util::py_float(parts[0])?;
    let lat = super::util::py_float(parts[1])?;
    if !lon.is_finite() || !lat.is_finite() || !(-180.0..=180.0).contains(&lon) || !(-90.0..=90.0).contains(&lat) {
        return None;
    }
    Some(Near { lon, lat })
}

/// Validates a `{w,s,e,n}` bounds object: the error text or the bounds.
pub fn validate_tile_bounds(bounds: Option<&Value>) -> Result<(f64, f64, f64, f64), String> {
    let Some(obj) = bounds.and_then(Value::as_object) else {
        return Err("bounds must be an object {w,s,e,n}".into());
    };
    let mut v = [0.0f64; 4];
    for (i, key) in ["w", "s", "e", "n"].iter().enumerate() {
        let n = match obj.get(*key) {
            Some(Value::Number(n)) => n.as_f64().filter(|f| f.is_finite()),
            _ => None,
        };
        match n {
            Some(f) => v[i] = f,
            None => return Err(format!("bounds.{key} must be a finite number")),
        }
    }
    let [w, s, e, n] = v;
    if !(-180.0..=180.0).contains(&w) || !(-180.0..=180.0).contains(&e) {
        return Err("bounds w/e must be in [-180, 180]".into());
    }
    if !(-90.0..=90.0).contains(&s) || !(-90.0..=90.0).contains(&n) {
        return Err("bounds s/n must be in [-90, 90]".into());
    }
    if w > e || s > n {
        return Err("bounds must satisfy w<=e and s<=n".into());
    }
    Ok((w, s, e, n))
}

/// Validates `minzoom` and `maxzoom` (whole numbers in 0 to 22, min not above
/// max, within the source cap). The error text, or the two zooms.
pub fn validate_tile_zooms(
    minzoom: Option<&Value>,
    maxzoom: Option<&Value>,
    src_maxzoom: i64,
) -> Result<(i64, i64), String> {
    let num = |v: Option<&Value>, name: &str| match v {
        Some(Value::Number(n)) => n.as_f64().ok_or_else(|| format!("{name} must be a number")),
        _ => Err(format!("{name} must be a number")),
    };
    let lo = num(minzoom, "minzoom")?;
    let hi = num(maxzoom, "maxzoom")?;
    let in_range = |x: f64| x > -1.0 && x < 23.0;
    if !in_range(lo) || !in_range(hi) {
        return Err("zooms must be in [0, 22]".into());
    }
    if lo != lo.trunc() || hi != hi.trunc() {
        return Err("zooms must be integers".into());
    }
    let (lo, hi) = (lo as i64, hi as i64);
    if !(0..=22).contains(&lo) || !(0..=22).contains(&hi) {
        return Err("zooms must be in [0, 22]".into());
    }
    if lo > hi {
        return Err("minzoom must be <= maxzoom".into());
    }
    if hi > src_maxzoom {
        return Err(format!("maxzoom exceeds source max ({src_maxzoom})"));
    }
    Ok((lo, hi))
}
