//! The scenes (`data/scenes.json`): one picture each, as data. What a scene
//! does not say is as `defaults` has it.

use serde_json::{json, Map, Value};

use crate::data;
use crate::flight::{self, Model, HOP_START};
use crate::vehicle::Options;

/// The interface is laid out in this many points whatever the screen has room for.
/// When a setup page is opened: the vehicle is there and its version is known.
pub const PAGE_AT: f64 = 11.0;
pub const FRAME: (f64, f64) = (1600.0, 1000.0);

pub struct Scene(pub Map<String, Value>);

fn defaults() -> Value {
    json!({
        "theme": "light-orange", "page": "home", "panel": "link", "airframe": "quad_x", "sensors": false,
        "full_table": false, "path": "orbit", "path_options": {}, "takeoff": 10.0, "armed": true,
        "mode": "MISSION", "chatter": true, "hud_controls": true, "settle": 20.0, "steps": [],
        "home": [HOP_START.0, HOP_START.1],
        "transmitter": {"throttle": 0.55, "rssi": 96,
            "switches": [[5, 1, 3], [6, 0, 2], [7, 0, 2], [8, 0, 2], [9, 1, 3], [10, 0, 2]],
            "knobs": [[11, 0.5], [12, 0.25]]},
    })
}

fn file() -> Value {
    serde_json::from_str(include_str!("../data/scenes.json")).expect("scenes.json")
}

/// The plan the mission planner opens with.
pub fn mission() -> Value {
    file()["mission"].clone()
}

pub fn all() -> Vec<Scene> {
    let base = defaults();
    file()["scenes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_object)
        .map(|o| {
            let mut m = base.as_object().cloned().unwrap_or_default();
            m.extend(o.clone());
            Scene(m)
        })
        .collect()
}

/// What an aircraft says on a normal sortie.
fn chatter(mode: &str) -> Vec<(f64, i64, String)> {
    [
        (1.0, 6, "[logger] file: /fs/microsd/log/2026-09-16"),
        (2.0, 6, "Using GPS 1 (NMEA, RTK fixed)"),
        (3.5, 6, "EKF2 GPS checks passed"),
        (5.0, 6, "Preflight checks passed"),
        (7.0, 5, "Armed by internal command"),
        (9.0, 6, &format!("Takeoff detected, mode {mode}")),
        (16.0, 6, "Takeoff altitude reached"),
        (22.0, 6, "Executing mission item 1"),
        (34.0, 6, "Executing mission item 2"),
        (41.0, 4, "RTK base link degraded, float fix"),
        (48.0, 6, "RTK fixed"),
        (57.0, 6, "Executing mission item 3"),
    ]
    .into_iter()
    .map(|(at, s, t)| (at, s, t.to_string()))
    .collect()
}

impl Scene {
    pub fn text(&self, key: &str) -> &str {
        self.0.get(key).and_then(Value::as_str).unwrap_or("")
    }

    pub fn number(&self, key: &str) -> f64 {
        self.0.get(key).and_then(Value::as_f64).unwrap_or(0.0)
    }

    pub fn flag(&self, key: &str) -> bool {
        self.0.get(key) == Some(&Value::Bool(true))
    }

    pub fn get(&self, key: &str) -> &Value {
        self.0.get(key).unwrap_or(&Value::Null)
    }

    pub fn id(&self) -> &str {
        self.text("id")
    }

    /// One field of the scene set from the command line, as JSON where it reads as that.
    pub fn set(&mut self, pair: &str) -> Result<(), String> {
        let (key, raw) = pair.split_once('=').ok_or(format!("{pair}: key=value is wanted"))?;
        const MORE: [&str; 12] = [
            "language",
            "plugin",
            "plugin_settings",
            "pause_after",
            "logs",
            "review_log",
            "vehicle_name",
            "geofence",
            "fleet",
            "terrain",
            "three_d",
            "params",
        ];
        if !self.0.contains_key(key) && !MORE.contains(&key) {
            return Err(format!("a scene has no field called {key}"));
        }
        let value = serde_json::from_str(raw).unwrap_or_else(|_| json!(raw));
        self.0.insert(key.into(), value);
        Ok(())
    }

    fn home_of(v: &Value) -> (f64, f64) {
        (
            v[0].as_f64().unwrap_or(HOP_START.0),
            v[1].as_f64().unwrap_or(HOP_START.1),
        )
    }

    pub fn model(&self) -> Result<Model, String> {
        let home = Scene::home_of(self.get("home"));
        let path = flight::path(self.text("path"), home, self.get("path_options"))?;
        Ok(Model::new(path, home, self.number("takeoff")))
    }

    /// The aircraft of the scene.
    pub fn vehicle(&self) -> Result<Options, String> {
        let none = Map::new();
        let set = self.get("params").as_object().unwrap_or(&none);
        let mode = self.text("mode").to_string();
        Ok(Options {
            params: data::parameters(
                self.text("airframe"),
                self.flag("sensors"),
                self.flag("full_table"),
                set,
            )?,
            model: self.model()?,
            armed: self.flag("armed"),
            chatter: if self.flag("chatter") && self.flag("armed") {
                chatter(&mode)
            } else {
                Vec::new()
            },
            mode,
            rover: false,
            transmitter: self.get("transmitter").clone(),
            logs: self.logs(),
            pause_after: self.get("pause_after").as_u64().map(|n| n as usize),
        })
    }

    /// The other vehicles, each on a link of its own.
    pub fn fleet(&self) -> Result<Vec<(String, String, Options)>, String> {
        let mut out = Vec::new();
        for f in self.get("fleet").as_array().into_iter().flatten() {
            let s = |k: &str, d: &str| f[k].as_str().unwrap_or(d).to_string();
            let home = Scene::home_of(&f["home"]);
            let path = flight::path(&s("path", "orbit"), home, &f["path_options"])?;
            let mut o = self.vehicle()?;
            o.model = Model::new(path, home, f["takeoff_time"].as_f64().unwrap_or(8.0));
            o.rover = s("kind", "drone") == "rover";
            o.chatter = Vec::new();
            o.logs = Vec::new();
            out.push((s("name", ""), s("color", ""), o));
        }
        Ok(out)
    }

    /// A morning's flights on the card, the newest a real log.
    fn logs(&self) -> Vec<(i64, i64, usize, Option<Vec<u8>>)> {
        let n = self.number("logs") as i64;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);
        (0..n)
            .map(|i| {
                let minutes = 4 + (i * 7) % 23;
                let size = (minutes * 1_150_000 + i * 37_000) as usize;
                let utc = now - n * 3600 + i * 1800;
                if i + 1 < n {
                    return (i + 1, utc, size, None);
                }
                let log = data::flight_log();
                (i + 1, utc, log.len(), Some(log))
            })
            .collect()
    }

    /// West, south, east and north of everything the map must hold, with a margin.
    pub fn bounds(&self) -> Result<[f64; 4], String> {
        let mut b = self.model()?.bounds();
        let mut take = |o: [f64; 4]| {
            b = [b[0].min(o[0]), b[1].min(o[1]), b[2].max(o[2]), b[3].max(o[3])];
        };
        for c in self.get("geofence").as_array().into_iter().flatten() {
            let (lon, lat) = (c[0].as_f64().unwrap_or(0.0), c[1].as_f64().unwrap_or(0.0));
            take([lon, lat, lon, lat]);
        }
        for (_, _, o) in self.fleet()? {
            take(o.model.bounds());
        }
        let (x, y) = (((b[2] - b[0]) * 0.18).max(0.0009), ((b[3] - b[1]) * 0.18).max(0.0006));
        Ok([b[0] - x, b[1] - y, b[2] + x, b[3] + y])
    }

    /// The page the window opens on: a setup page is opened later, when there is a vehicle to read.
    pub fn first_page(&self) -> &str {
        let page = self.text("page");
        let top = ["home", "logs", "setup"].contains(&page) || page.contains(':');
        if top {
            page
        } else {
            "setup"
        }
    }

    /// The interface's half: the frame, and the steps at their times, the map framed last.
    pub fn staging(&self, shot_at: f64) -> Result<Value, String> {
        let mut steps: Vec<Value> = Vec::new();
        if self.first_page() == "setup" && self.text("page") != "setup" {
            steps.push(json!({"at": PAGE_AT, "open": format!("setup:{}", self.text("page"))}));
        }
        let panel = self.text("panel");
        if panel != "collapsed" {
            steps.push(json!({"at": 2.0, "panel": panel}));
        }
        if !self.text("plugin").is_empty() {
            steps.push(json!({"at": 7.0, "open": format!("plugin:{}", self.text("plugin"))}));
        }
        steps.extend(self.get("steps").as_array().cloned().unwrap_or_default());
        if self.text("page") == "home" && !self.flag("no_fit") {
            steps.push(json!({"at": shot_at - 12.0, "fit": self.bounds()?}));
        }
        if self.0.contains_key("three_d") {
            steps.push(json!({"at": shot_at - 10.0, "three_d": self.get("three_d")}));
        }
        Ok(json!({
            "frame": [FRAME.0, FRAME.1],
            "panel": if panel == "collapsed" { "collapsed" } else { "open" },
            "hud_controls": self.flag("hud_controls"),
            "steps": steps,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_scene_builds() {
        let mut ids: Vec<String> = Vec::new();
        for s in all() {
            assert!(!s.id().is_empty() && !ids.contains(&s.id().to_string()), "{}", s.id());
            ids.push(s.id().to_string());
            let v = s.vehicle().unwrap_or_else(|e| panic!("{}: {e}", s.id()));
            assert!(v.params.len() > 100, "{}", s.id());
            s.fleet().unwrap();
            let b = s.bounds().unwrap();
            assert!(b[0] < b[2] && b[1] < b[3], "{}", s.id());
            assert!(s.staging(60.0).unwrap()["steps"].is_array());
        }
        assert!(ids.len() > 30);
    }

    /// A picture the website shows is one a scene makes, and the other way round.
    #[test]
    fn the_website_shows_what_the_scenes_make() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let page = std::fs::read_to_string(root.join("docs/index.html")).unwrap();
        let made: Vec<String> = all()
            .iter()
            .map(|s| s.text("web").to_string())
            .filter(|w| !w.is_empty())
            .collect();
        for part in page.split("data-full=\"assets/images/").skip(1) {
            let name = part.split(".jpg").next().unwrap_or("");
            assert!(made.iter().any(|m| m == name), "no scene makes {name}");
        }
        for name in &made {
            assert!(root.join(format!("docs/assets/images/{name}.jpg")).is_file(), "{name}");
        }
    }

    #[test]
    fn a_field_is_set_from_the_command_line() {
        let mut s = all().remove(0);
        s.set("theme=blue").unwrap();
        s.set("armed=false").unwrap();
        s.set(r#"path_options={"alt": 70}"#).unwrap();
        assert_eq!((s.text("theme"), s.flag("armed")), ("blue", false));
        assert_eq!(s.get("path_options")["alt"], 70);
        assert!(s.set("colour=red").is_err() && s.set("theme").is_err());
    }
}
