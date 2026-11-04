//! The place search of the Home map (`python/src/js/map-search.js`, map.css
//! `.map-search*`): a coordinate pair is read here in every notation a
//! briefing writes one in, and answers at once with no network; a downloaded
//! area is matched by name from this laptop; any other name goes to
//! `/api/geocode`. This module is the rules, `map_search_ui` draws them.

use serde_json::Value;

use crate::map::regions::{Bounds, Region};

pub const MAX_ROWS: usize = 7;
pub const RESULT_ZOOM: f64 = 15.0;
pub const MAX_ZOOM: f64 = 16.0;

/// One answer in the list.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub label: String,
    pub kind: String,
    /// This laptop's own answer, not the geocoder's.
    pub local: bool,
    pub lat: f64,
    pub lon: f64,
    pub bounds: Option<Bounds>,
}

/// Degrees from one group of numbers: degrees, minutes, seconds.
fn dms_to_degrees(parts: &[f64]) -> Option<f64> {
    let degrees = parts[0].abs();
    let minutes = parts.get(1).copied().unwrap_or(0.0);
    let seconds = parts.get(2).copied().unwrap_or(0.0);
    if !(0.0..60.0).contains(&minutes) || !(0.0..60.0).contains(&seconds) {
        return None;
    }
    let v = degrees + minutes / 60.0 + seconds / 3600.0;
    Some(if parts[0] < 0.0 || (parts[0] == 0.0 && parts[0].is_sign_negative()) {
        -v
    } else {
        v
    })
}

#[derive(Debug, Default, Clone)]
struct Group {
    nums: Vec<f64>,
    hemi: Option<char>,
}

/// One side of a pair, tokenised; a hemisphere letter ends the group it
/// belongs to, so "N48 E11" and "48N 11E" are the same two numbers.
fn coordinate_groups(text: &str) -> Vec<Group> {
    let mut groups = Vec::new();
    let mut cur = Group::default();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let hemi = matches!(c, 'N' | 'S' | 'E' | 'W' | 'n' | 's' | 'e' | 'w');
        let starts_number =
            c.is_ascii_digit() || ((c == '-' || c == '+') && chars.get(i + 1).is_some_and(char::is_ascii_digit));
        if !hemi && !starts_number {
            i += 1;
            continue;
        }
        if cur.hemi.is_some() && (hemi || !cur.nums.is_empty()) {
            groups.push(std::mem::take(&mut cur));
        }
        if hemi {
            cur.hemi = Some(c.to_ascii_uppercase());
            i += 1;
        } else {
            let start = i;
            i += 1;
            while i < chars.len() && chars[i].is_ascii_digit() {
                i += 1;
            }
            if i + 1 < chars.len() && chars[i] == '.' && chars[i + 1].is_ascii_digit() {
                i += 1;
                while i < chars.len() && chars[i].is_ascii_digit() {
                    i += 1;
                }
            }
            let s: String = chars[start..i].iter().collect();
            cur.nums.push(s.parse().unwrap_or(f64::NAN));
        }
    }
    if !cur.nums.is_empty() || cur.hemi.is_some() {
        groups.push(cur);
    }
    groups
}

/// A typed coordinate pair as (latitude, longitude), or `None` if it is not
/// one. Strict on purpose: a place name that half parsed as numbers would fly
/// the operator to the Atlantic instead of saying "no network".
pub fn parse_coordinates(text: &str) -> Option<(f64, f64)> {
    let mut raw = text.trim().to_string();
    if raw.is_empty() {
        return None;
    }
    let two_german_decimals = {
        let parts: Vec<&str> = raw.split_whitespace().collect();
        parts.len() == 2
            && parts.iter().all(|p| {
                let p = p.trim_start_matches(['-', '+']);
                match p.split_once(',') {
                    Some((a, b)) => {
                        !a.is_empty()
                            && !b.is_empty()
                            && a.bytes().all(|c| c.is_ascii_digit())
                            && b.bytes().all(|c| c.is_ascii_digit())
                    }
                    None => false,
                }
            })
    };
    if two_german_decimals {
        raw = raw.replace(',', ".");
    }
    let allowed = |c: char| {
        c.is_ascii_digit()
            || c.is_whitespace()
            || matches!(
                c,
                '.' | ',' | '-' | '+' | '\u{b0}' | '\u{ba}' | '\'' | '\u{2032}' | '"' | '\u{2033}' | '/'
            )
            || matches!(c, 'N' | 'S' | 'E' | 'W' | 'n' | 's' | 'e' | 'w')
    };
    if !raw.chars().all(allowed) {
        return None;
    }
    let sides: Vec<&str> = raw.split([',', '/']).map(str::trim).filter(|s| !s.is_empty()).collect();
    let groups: Vec<Group> = if sides.len() == 2 {
        let (l, r) = (coordinate_groups(sides[0]), coordinate_groups(sides[1]));
        if l.len() != 1 || r.len() != 1 {
            return None;
        }
        vec![l[0].clone(), r[0].clone()]
    } else if sides.len() == 1 {
        let mut g = coordinate_groups(sides[0]);
        if g.len() == 1 && g[0].hemi.is_none() {
            let nums = g[0].nums.clone();
            if nums.len() < 2 || nums.len() > 6 || !nums.len().is_multiple_of(2) {
                return None;
            }
            let half = nums.len() / 2;
            g = vec![
                Group {
                    nums: nums[..half].to_vec(),
                    hemi: None,
                },
                Group {
                    nums: nums[half..].to_vec(),
                    hemi: None,
                },
            ];
        }
        g
    } else {
        return None;
    };
    if groups.len() != 2 {
        return None;
    }
    if groups
        .iter()
        .any(|g| g.nums.is_empty() || g.nums.len() > 3 || g.nums.iter().any(|n| !n.is_finite()))
    {
        return None;
    }
    let mut values = Vec::new();
    for g in &groups {
        let d = dms_to_degrees(&g.nums)?;
        values.push(if matches!(g.hemi, Some('S' | 'W')) { -d.abs() } else { d });
    }
    let hemis: Vec<Option<char>> = groups.iter().map(|g| g.hemi).collect();
    let is_ns = |h: &Option<char>| matches!(h, Some('N' | 'S'));
    let is_ew = |h: &Option<char>| matches!(h, Some('E' | 'W'));
    let (mut lat, mut lon);
    if hemis.iter().any(is_ns) && hemis.iter().any(is_ew) {
        let lat_index = if is_ns(&hemis[0]) { 0 } else { 1 };
        lat = values[lat_index];
        lon = values[1 - lat_index];
    } else {
        lat = values[0];
        lon = values[1];
        if lat.abs() > 90.0 && lon.abs() <= 90.0 {
            std::mem::swap(&mut lat, &mut lon);
        }
    }
    if !lat.is_finite() || !lon.is_finite() || lat.abs() > 90.0 || lon.abs() > 180.0 {
        return None;
    }
    Some((lat, lon))
}

/// How a pair reads back, so the row says what was understood.
pub fn format_coordinates(lat: f64, lon: f64) -> String {
    format!("{lat:.6}\u{b0}, {lon:.6}\u{b0}")
}

/// A geocoder label split into the place and its address, at the first comma.
pub fn split_label(label: &str) -> (String, String) {
    let text = label.trim();
    match text.find(',') {
        Some(i) if i > 0 => (text[..i].trim().to_string(), text[i + 1..].trim().to_string()),
        _ => (text.to_string(), String::new()),
    }
}

/// Which row is selected after the list is drawn again: the same label if it
/// is still there, else the first.
pub fn keep_selection(previous: Option<&str>, rows: &[Row]) -> Option<usize> {
    if rows.is_empty() {
        return None;
    }
    Some(
        previous
            .and_then(|p| rows.iter().position(|r| r.label == p))
            .unwrap_or(0),
    )
}

/// Downloaded areas whose name matches, as rows.
pub fn region_matches(regions: &[Region], query: &str) -> Vec<Row> {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return Vec::new();
    }
    let mut seen: Vec<String> = Vec::new();
    let mut rows = Vec::new();
    for r in regions {
        let name = r.name.trim();
        let lower = name.to_lowercase();
        if name.is_empty() || seen.contains(&lower) || !lower.contains(&needle) {
            continue;
        }
        seen.push(lower);
        rows.push(Row {
            label: name.to_string(),
            kind: "downloaded area".into(),
            local: true,
            lat: (r.bounds.s + r.bounds.n) / 2.0,
            lon: (r.bounds.w + r.bounds.e) / 2.0,
            bounds: Some(r.bounds),
        });
    }
    rows
}

/// What this laptop answers on its own: a coordinate pair, then areas.
pub fn local_rows(regions: &[Region], query: &str) -> Vec<Row> {
    let text = query.trim();
    if text.is_empty() {
        return Vec::new();
    }
    let mut rows = Vec::new();
    if let Some((lat, lon)) = parse_coordinates(text) {
        rows.push(Row {
            label: format_coordinates(lat, lon),
            kind: "coordinates".into(),
            local: true,
            lat,
            lon,
            bounds: None,
        });
    }
    rows.extend(region_matches(regions, text));
    rows.truncate(MAX_ROWS);
    rows
}

/// The rows of an `/api/geocode` answer, and its error line.
pub fn read_geocode(doc: &Value) -> (Vec<Row>, String) {
    let rows = doc
        .get("results")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|r| {
                    let lat = r.get("lat").and_then(Value::as_f64)?;
                    let lon = r.get("lon").and_then(Value::as_f64)?;
                    let b = r.get("bounds").filter(|b| b.is_object());
                    let n = |k: &str| b.and_then(|b| b.get(k)).and_then(Value::as_f64);
                    let bounds = match (n("w"), n("s"), n("e"), n("n")) {
                        (Some(w), Some(s), Some(e), Some(nn)) => Some(Bounds { w, s, e, n: nn }),
                        _ => None,
                    };
                    Some(Row {
                        label: r.get("label").and_then(Value::as_str).unwrap_or("").to_string(),
                        kind: r.get("kind").and_then(Value::as_str).unwrap_or("").to_string(),
                        local: false,
                        lat,
                        lon,
                        bounds,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let error = doc.get("error").and_then(Value::as_str).unwrap_or("").to_string();
    (rows, error)
}

/// The zoom that frames `b` in a view `width` by `height` px with `padding`
/// on every side, never closer than `max` (Web Mercator, 256 px tiles).
pub fn fit_zoom(b: &Bounds, width: f64, height: f64, padding: f64, max: f64) -> f64 {
    let x = |lng: f64| (lng + 180.0) / 360.0;
    let y = |lat: f64| {
        let s = lat.to_radians().sin().clamp(-0.9999, 0.9999);
        0.5 - ((1.0 + s) / (1.0 - s)).ln() / (4.0 * std::f64::consts::PI)
    };
    let dx = (x(b.e) - x(b.w)).abs().max(1e-12);
    let dy = (y(b.s) - y(b.n)).abs().max(1e-12);
    let zx = ((width - 2.0 * padding).max(1.0) / (256.0 * dx)).log2();
    let zy = ((height - 2.0 * padding).max(1.0) / (256.0 * dy)).log2();
    zx.min(zy).clamp(0.0, max)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn near(a: Option<(f64, f64)>, lat: f64, lon: f64) {
        let (la, lo) = a.unwrap_or_else(|| panic!("no pair for {lat} {lon}"));
        assert!((la - lat).abs() < 1e-5 && (lo - lon).abs() < 1e-5, "{la}, {lo}");
    }

    #[test]
    fn every_notation_of_a_pair_is_read() {
        near(parse_coordinates("48.0802 11.6409"), 48.0802, 11.6409);
        near(parse_coordinates("48.08, 11.64"), 48.08, 11.64);
        near(parse_coordinates("48,080217 11,640969"), 48.080217, 11.640969);
        near(parse_coordinates("N48.08 E11.64"), 48.08, 11.64);
        near(parse_coordinates("48 04.8 N 11 38.4 E"), 48.08, 11.64);
        near(parse_coordinates("11E 48N"), 48.0, 11.0);
        near(parse_coordinates("48 04 48.8 11 38 27.5"), 48.080222, 11.640972);
        near(
            parse_coordinates("48\u{b0}04'48.8\"N 11\u{b0}38'27.5\"E"),
            48.080222,
            11.640972,
        );
        near(parse_coordinates("-33.9 151.2"), -33.9, 151.2);
        near(parse_coordinates("33.9S 151.2E"), -33.9, 151.2);
        near(parse_coordinates("120 40"), 40.0, 120.0);
    }

    #[test]
    fn a_name_is_never_a_position() {
        for s in [
            "",
            "Neubiberg",
            "N 48",
            "48",
            "91 200",
            "48.1 11.6 5",
            "Munich 48 11",
            "48 99 11 20",
        ] {
            assert_eq!(parse_coordinates(s), None, "{s}");
        }
    }

    #[test]
    fn labels_split_at_the_first_comma() {
        assert_eq!(
            split_label("Neubiberg, Landkreis M\u{fc}nchen, Bayern"),
            ("Neubiberg".into(), "Landkreis M\u{fc}nchen, Bayern".into())
        );
        assert_eq!(split_label("Oslo"), ("Oslo".into(), String::new()));
    }

    #[test]
    fn a_downloaded_area_is_found_by_part_of_its_name() {
        let r = |name: &str| Region {
            name: name.into(),
            source: "osm".into(),
            bounds: Bounds {
                w: 11.0,
                s: 48.0,
                e: 11.2,
                n: 48.2,
            },
            minzoom: Some(8.0),
            maxzoom: Some(15.0),
            running: false,
        };
        let rows = region_matches(&[r("Manching field"), r("manching field"), r("Other")], "MANCH");
        assert_eq!(rows.len(), 1);
        assert!((rows[0].lat - 48.1).abs() < 1e-9 && rows[0].kind == "downloaded area");
        let local = local_rows(&[r("48 field")], "48 11");
        assert_eq!(local[0].kind, "coordinates");
    }

    #[test]
    fn the_selection_follows_the_row_it_was_on() {
        let row = |l: &str| Row {
            label: l.into(),
            kind: String::new(),
            local: true,
            lat: 0.0,
            lon: 0.0,
            bounds: None,
        };
        let rows = vec![row("a"), row("b")];
        assert_eq!(keep_selection(Some("b"), &rows), Some(1));
        assert_eq!(keep_selection(Some("z"), &rows), Some(0));
        assert_eq!(keep_selection(None, &[]), None);
    }

    #[test]
    fn a_geocoder_answer_is_read() {
        let doc = json!({"ok": true, "results": [
            {"label": "A, B", "lat": 48.1, "lon": 11.6, "kind": "city", "bounds": {"w": 11.5, "s": 48.0, "e": 11.7, "n": 48.2}},
            {"label": "no position"}
        ], "error": ""});
        let (rows, e) = read_geocode(&doc);
        assert_eq!(rows.len(), 1);
        assert!(rows[0].bounds.is_some() && e.is_empty());
        assert_eq!(
            read_geocode(&json!({"results": [], "error": "No network"})).1,
            "No network"
        );
    }

    #[test]
    fn a_box_is_framed_with_room_around_it() {
        let b = Bounds {
            w: 11.0,
            s: 48.0,
            e: 11.1,
            n: 48.1,
        };
        let z = fit_zoom(&b, 1000.0, 800.0, 80.0, 16.0);
        assert!((11.0..13.0).contains(&z), "{z}");
        let tiny = Bounds {
            w: 11.0,
            s: 48.0,
            e: 11.00001,
            n: 48.00001,
        };
        assert_eq!(fit_zoom(&tiny, 1000.0, 800.0, 80.0, 16.0), 16.0);
    }
}
