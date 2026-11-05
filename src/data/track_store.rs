//! The tracks kept over a restart (`saveTrack` and `restoreTrack` of
//! `python/src/js/map.js`, `save` and `restore` of `map-fleet.js`): the flown
//! track and the track of every other vehicle, in `~/.corvus/gui-tracks.json`,
//! which belongs to this station and not to the aircraft. They are read once
//! at start (unless Settings clears the track on restart) and written a few
//! seconds after they change.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::{json, Value};

use crate::data::track::Point;

/// A line is not a line below this many points.
const MIN_LINE: usize = 2;
/// The same cap the live track has.
const MAX_POINTS: usize = 20000;

pub type Flights = (Vec<Point>, Vec<Vec<Point>>);

/// What the file holds.
#[derive(Debug, Default, PartialEq)]
pub struct Stored {
    pub main: Flights,
    pub fleet: BTreeMap<String, Flights>,
}

pub fn default_path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".corvus").join("gui-tracks.json"))
}

fn point(p: &Point) -> Value {
    let r7 = |v: f64| (v * 1e7).round() / 1e7;
    match p.alt {
        Some(a) => json!([r7(p.lng), r7(p.lat), (a * 10.0).round() / 10.0]),
        None => json!([r7(p.lng), r7(p.lat)]),
    }
}

fn flight(f: &[Point]) -> Value {
    Value::Array(f.iter().map(point).collect())
}

fn flights(f: &Flights) -> Value {
    json!({"current": flight(&f.0), "past": f.1.iter().map(|p| flight(p)).collect::<Vec<_>>()})
}

fn read_flight(v: &Value) -> Vec<Point> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|p| {
                    let p = p.as_array()?;
                    let (lng, lat) = (p.first()?.as_f64()?, p.get(1)?.as_f64()?);
                    (lng.is_finite() && lat.is_finite()).then(|| Point {
                        lng,
                        lat,
                        alt: p.get(2).and_then(Value::as_f64),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn read_flights(v: &Value) -> Flights {
    let current = read_flight(v.get("current").unwrap_or(&Value::Null));
    let past = v
        .get("past")
        .and_then(Value::as_array)
        .map(|a| a.iter().map(read_flight).filter(|f| f.len() >= MIN_LINE).collect())
        .unwrap_or_default();
    (current, past)
}

/// The file's text for these tracks. Nothing to keep is no text at all.
pub fn encode(main: &Flights, fleet: &BTreeMap<String, Flights>) -> Option<String> {
    let has = |f: &Flights| f.0.len() >= MIN_LINE || !f.1.is_empty();
    let fleet: serde_json::Map<String, Value> = fleet
        .iter()
        .filter(|(_, f)| has(f))
        .map(|(id, f)| (id.clone(), flights(f)))
        .collect();
    if !has(main) && fleet.is_empty() {
        return None;
    }
    Some(json!({"v": 1, "main": flights(main), "fleet": fleet}).to_string())
}

/// What a file's text holds; a file that is not ours is nothing.
pub fn decode(text: &str) -> Stored {
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return Stored::default();
    };
    if v.get("v") != Some(&json!(1)) {
        return Stored::default();
    }
    let cap = |f: Flights| -> Flights {
        let (mut current, mut past) = f;
        let mut total = current.len() + past.iter().map(Vec::len).sum::<usize>();
        while total > MAX_POINTS && !past.is_empty() {
            total -= past.remove(0).len();
        }
        if total > MAX_POINTS {
            current.drain(..total - MAX_POINTS);
        }
        (current, past)
    };
    Stored {
        main: cap(read_flights(v.get("main").unwrap_or(&Value::Null))),
        fleet: v
            .get("fleet")
            .and_then(Value::as_object)
            .map(|o| {
                o.iter()
                    .map(|(id, f)| (id.clone(), cap(read_flights(f))))
                    .filter(|(_, f)| f.0.len() >= MIN_LINE || !f.1.is_empty())
                    .collect()
            })
            .unwrap_or_default(),
    }
}

pub fn load(path: &PathBuf) -> Stored {
    std::fs::read_to_string(path).map(|t| decode(&t)).unwrap_or_default()
}

/// Writes the tracks, or removes the file when there is nothing to keep.
pub fn save(path: &PathBuf, main: &Flights, fleet: &BTreeMap<String, Flights>) {
    match encode(main, fleet) {
        Some(text) => {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(path, text);
        }
        None => {
            let _ = std::fs::remove_file(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(n: usize) -> Vec<Point> {
        (0..n)
            .map(|i| Point {
                lng: 11.64 + i as f64 * 1e-4,
                lat: 48.07,
                alt: (i % 2 == 0).then_some(12.34),
            })
            .collect()
    }

    #[test]
    fn what_is_saved_comes_back() {
        let main = (line(5), vec![line(3)]);
        let mut fleet = BTreeMap::new();
        fleet.insert("link-2:1".to_string(), (line(4), vec![]));
        let text = encode(&main, &fleet).expect("something to keep");
        let back = decode(&text);
        assert_eq!(back.main.0.len(), 5);
        assert_eq!(back.main.1.len(), 1);
        assert_eq!(back.main.0[0].alt, Some(12.3));
        assert_eq!(back.main.0[1].alt, None);
        assert!((back.main.0[2].lng - 11.6402).abs() < 1e-9);
        assert_eq!(back.fleet["link-2:1"].0.len(), 4);
    }

    #[test]
    fn nothing_worth_keeping_is_no_file() {
        assert_eq!(encode(&(vec![], vec![]), &BTreeMap::new()), None);
        assert_eq!(
            encode(&(line(1), vec![]), &BTreeMap::new()),
            None,
            "one point is not a line"
        );
        let mut fleet = BTreeMap::new();
        fleet.insert("a".to_string(), (line(1), vec![]));
        assert_eq!(encode(&(vec![], vec![]), &fleet), None);
    }

    #[test]
    fn a_file_that_is_not_ours_is_nothing() {
        assert_eq!(decode("not json"), Stored::default());
        assert_eq!(
            decode(r#"{"v": 2, "main": {"current": [[1,2],[3,4]]}}"#),
            Stored::default()
        );
        assert_eq!(decode("{}"), Stored::default());
    }

    #[test]
    fn bad_points_and_short_flights_are_dropped() {
        let text = r#"{"v":1,"main":{"current":[[11.6,48.1],["x",1],[11.7,48.1]],"past":[[[1,1]],[[1,1],[2,2]]]},"fleet":{"a":{"current":[[1,1]],"past":[]}}}"#;
        let s = decode(text);
        assert_eq!(s.main.0.len(), 2);
        assert_eq!(s.main.1.len(), 1);
        assert!(s.fleet.is_empty());
    }

    #[test]
    fn the_oldest_flights_go_first_past_the_cap() {
        let big = (line(15000), vec![line(6000), line(6000)]);
        let s = decode(&encode(&big, &BTreeMap::new()).expect("kept"));
        let total = s.main.0.len() + s.main.1.iter().map(Vec::len).sum::<usize>();
        assert!(total <= MAX_POINTS);
        assert_eq!(s.main.0.len(), 15000);
        assert_eq!(s.main.1.len(), 0);
    }

    #[test]
    fn it_is_written_and_read_through_a_file() {
        let dir = std::env::temp_dir().join(format!("corvus-tracks-{}", std::process::id()));
        let path = dir.join("t.json");
        save(&path, &(line(3), vec![]), &BTreeMap::new());
        assert_eq!(load(&path).main.0.len(), 3);
        save(&path, &(vec![], vec![]), &BTreeMap::new());
        assert!(!path.exists());
        assert_eq!(load(&path), Stored::default());
        let _ = std::fs::remove_dir_all(dir);
    }
}
