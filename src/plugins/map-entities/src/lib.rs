//! Map Entities: a tactical layer on the Home map
//! (`python/plugins/map-entities/map-entities.js` and `.css`).
//!
//! Things the operator places by hand at an exact coordinate: gates (two
//! posts and the opening between them), trees (a crown radius and a height)
//! and a dozen symbols with an optional radius, drawn as a light area (a
//! radar's reach, a blast's danger zone), a full circle or a sector. The
//! plugin watches the aircraft's track and says whether it flew through a
//! gate's opening, over it or under it, and whether it entered an area.
//!
//! What is placed is saved through `save_settings`; what the aircraft did is
//! kept for the session only, and "Reset results" starts it again.

pub mod model;
mod view;

use std::collections::HashMap;

use serde_json::{json, Value};

use self::model::{
    bearing_deg, make_entity, normalize_list, points_of, process_step, sample_of, sector_ring, status_color, Entity,
    Results, Sample, MAX_ENTITIES, MAX_STEP_M, MIN_STEP_M,
};
use corvus_api::plugin::options::OptionSpec;
use corvus_api::plugin::shapes::{self, circle_ring, Style};
use corvus_api::plugin::{Host, Info, MapEvent, Plugin};

const UNDO_SECS: f64 = 8.0;
const PENDING_KEY: &str = "pending";

/// The three switches of the plugin's gear.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Opts {
    pub labels: bool,
    pub notify: bool,
    pub armed_only: bool,
}

impl Default for Opts {
    fn default() -> Opts {
        Opts {
            labels: true,
            notify: true,
            armed_only: true,
        }
    }
}

/// A kind being placed: the next click on the map makes one.
#[derive(Debug, Clone, PartialEq)]
pub struct Placing {
    pub kind: &'static str,
    pub step: u8,
    pub first: Option<[f64; 2]>,
}

/// A click on the map that moves or aims one entity.
#[derive(Debug, Clone, PartialEq)]
pub enum Aim {
    Post(String, char),
    Bearing(String),
}

/// What a delete can still take back.
pub struct Undo {
    pub items: Vec<(Entity, usize)>,
    pub until: f64,
}

#[derive(Default)]
pub struct MapEntities {
    pub entities: Vec<Entity>,
    pub results: HashMap<String, Results>,
    pub selected: Option<String>,
    pub placing: Option<Placing>,
    aim: Option<Aim>,
    pub undo: Option<Undo>,
    prev: Option<Sample>,
    pub opts: Opts,
    started: bool,
    pub open: bool,
}

impl Plugin for MapEntities {
    fn info(&self) -> Info {
        Info {
            id: "map-entities",
            name: "Map Entities",
            icon: "map-pin",
            description: "Gates, trees, people and devices with a range, placed exactly on the Home map",
            version: "1.0.0",
            order: 35,
            can_tab: true,
        }
    }

    fn options(&self) -> Vec<OptionSpec> {
        vec![
            OptionSpec::toggle("labels", "Show names on the map", true),
            OptionSpec::toggle("notify", "Notify on gate passes and area entries", true),
            OptionSpec::toggle("armedOnly", "Only check while armed", true),
        ]
    }

    fn start(&mut self, h: &mut Host) {
        if self.started {
            return;
        }
        self.started = true;
        self.entities = normalize_list(h.settings().get("entities").unwrap_or(&Value::Null));
        self.read_options(h);
        self.draw_all(h);
    }

    fn tick(&mut self, h: &mut Host) {
        if let Some(u) = &self.undo {
            if h.now >= u.until {
                self.undo = None;
            } else {
                h.ctx.request_repaint_after(std::time::Duration::from_millis(500));
            }
        }
        self.on_state(h);
    }

    fn open(&mut self, _h: &mut Host) {
        self.open = true;
    }

    fn close(&mut self, h: &mut Host) {
        self.open = false;
        self.stop_placing(h);
        self.aim = None;
        self.undo = None;
    }

    fn options_changed(&mut self, h: &mut Host) {
        self.read_options(h);
        self.draw_all(h);
    }

    fn map_event(&mut self, e: MapEvent, h: &mut Host) {
        match e {
            MapEvent::Click(key) => {
                if let Some(id) = entity_of_key(&key) {
                    self.select(Some(id), h);
                }
            }
            MapEvent::Drag(key, at) => self.drag_preview(&key, at, h),
            MapEvent::DragEnd(key, at) => self.drag_end(&key, at, h),
            MapEvent::Picked(at) => self.picked(at, h),
            MapEvent::PickCancelled => {
                self.placing = None;
                self.aim = None;
                h.map().remove(PENDING_KEY);
            }
        }
    }

    fn show(&mut self, ui: &mut eframe::egui::Ui, area: eframe::egui::Rect, h: &mut Host) -> f32 {
        view::show(self, ui, area, h)
    }
}

/// `moveHandler`'s id: a shape's key is `<id>-<l|a|b|m|c>`.
fn entity_of_key(key: &str) -> Option<String> {
    let (id, suffix) = key.rsplit_once('-')?;
    matches!(suffix, "l" | "a" | "b" | "m" | "c").then(|| id.to_string())
}

impl MapEntities {
    fn read_options(&mut self, h: &Host) {
        let o = h.options();
        let on = |k: &str| o.get(k).and_then(Value::as_bool) != Some(false);
        self.opts = Opts {
            labels: on("labels"),
            notify: on("notify"),
            armed_only: on("armedOnly"),
        };
    }

    pub fn find(&self, id: &str) -> Option<&Entity> {
        self.entities.iter().find(|e| e.id == id)
    }

    fn find_mut(&mut self, id: &str) -> Option<&mut Entity> {
        self.entities.iter_mut().find(|e| e.id == id)
    }

    // ---- watching the aircraft ------------------------------------------------

    fn on_state(&mut self, h: &mut Host) {
        let Some(cur) = sample_of(h.state, self.opts.armed_only) else {
            self.prev = None;
            return;
        };
        let Some(prev) = self.prev else {
            self.prev = Some(cur);
            return;
        };
        let step = model::distance_m([prev.lng, prev.lat], [cur.lng, cur.lat]);
        if step > MAX_STEP_M {
            self.prev = Some(cur);
            return;
        }
        if step < MIN_STEP_M && (cur.alt - prev.alt).abs() < MIN_STEP_M {
            return;
        }
        let events = process_step(&self.entities, &mut self.results, prev, cur, h.now);
        self.prev = Some(cur);
        for ev in &events {
            if self.opts.notify {
                h.notify(ev.level, &ev.text);
            }
            if let Some(e) = self.find(&ev.entity).cloned() {
                self.draw_entity(&e, h);
            }
        }
    }

    pub fn reset_results(&mut self, h: &mut Host) {
        self.results.clear();
        self.draw_all(h);
    }

    // ---- drawing on the map ---------------------------------------------------

    fn undraw(&self, id: &str, h: &mut Host) {
        for s in ["l", "a", "b", "m", "c"] {
            h.map().remove(&format!("{id}-{s}"));
        }
    }

    pub fn draw_all(&self, h: &mut Host) {
        for e in &self.entities {
            self.draw_entity(e, h);
        }
    }

    pub fn draw_entity(&self, e: &Entity, h: &mut Host) {
        let color = shapes::hex(status_color(e, self.results.get(&e.id))).unwrap_or(shapes::DEFAULT_COLOR);
        let selected = self.selected.as_deref() == Some(e.id.as_str());
        let label = if self.opts.labels { e.name.as_str() } else { "" };
        let id = &e.id;
        let style = Style::color(color);
        let m = h.map();
        if e.is_gate() {
            m.remove(&format!("{id}-c"));
            m.line(
                &format!("{id}-l"),
                &[[e.lng, e.lat], [e.lng2, e.lat2]],
                style,
                Some(5.0),
                false,
            );
            for (suffix, at, which) in [("a", [e.lng, e.lat], "post A"), ("b", [e.lng2, e.lat2], "post B")] {
                m.marker(
                    &format!("{id}-{suffix}"),
                    at,
                    "",
                    "",
                    &format!("{}, {which}", e.name),
                    style,
                    Some(18.0),
                    selected,
                    selected,
                    true,
                );
            }
            m.marker(
                &format!("{id}-m"),
                [(e.lng + e.lng2) / 2.0, (e.lat + e.lat2) / 2.0],
                e.kind.id,
                label,
                &e.name,
                style,
                None,
                selected,
                false,
                true,
            );
            return;
        }
        for s in ["l", "a", "b"] {
            m.remove(&format!("{id}-{s}"));
        }
        let key = format!("{id}-c");
        if e.radius > 0.0 {
            let st = style.opacity(1.0);
            let dashed = !e.is_tree();
            let ring = if e.sector_angle() < 360.0 {
                sector_ring(e)
            } else {
                circle_ring([e.lng, e.lat], e.radius, None)
            };
            m.polygon(&key, &ring, st, Some(0.14), Some(2.5), dashed);
        } else {
            m.remove(&key);
        }
        m.marker(
            &format!("{id}-m"),
            [e.lng, e.lat],
            e.kind.id,
            label,
            &e.name,
            style,
            Some(if e.is_tree() { 24.0 } else { 28.0 }),
            selected,
            selected,
            true,
        );
    }

    /// A gate's post being dragged: the line follows it.
    fn drag_preview(&mut self, key: &str, at: [f64; 2], h: &mut Host) {
        let Some(id) = entity_of_key(key) else { return };
        let Some(e) = self.find(&id).cloned() else { return };
        if !e.is_gate() {
            return;
        }
        let color = shapes::hex(status_color(&e, self.results.get(&id))).unwrap_or(shapes::DEFAULT_COLOR);
        let (a, b) = if key.ends_with("-a") {
            (at, [e.lng2, e.lat2])
        } else {
            ([e.lng, e.lat], at)
        };
        h.map()
            .line(&format!("{id}-l"), &[a, b], Style::color(color), Some(5.0), false);
    }

    fn drag_end(&mut self, key: &str, at: [f64; 2], h: &mut Host) {
        let Some(id) = entity_of_key(key) else { return };
        let which = if key.ends_with("-b") { 'b' } else { 'a' };
        self.move_post(&id, which, at, h);
    }

    fn move_post(&mut self, id: &str, which: char, at: [f64; 2], h: &mut Host) {
        if let Some(e) = self.find_mut(id) {
            if which == 'b' {
                e.lng2 = at[0];
                e.lat2 = at[1];
            } else {
                e.lng = at[0];
                e.lat = at[1];
            }
        }
        if let Some(e) = self.find(id).cloned() {
            self.draw_entity(&e, h);
        }
        self.save(h);
    }

    // ---- saving ---------------------------------------------------------------

    pub fn save(&self, h: &mut Host) {
        let list: Vec<Value> = self.entities.iter().map(Entity::to_json).collect();
        let _ = h.save_settings(&json!({"entities": list}), false);
    }

    // ---- editing --------------------------------------------------------------

    pub fn select(&mut self, id: Option<String>, h: &mut Host) {
        let before = self.selected.clone();
        self.selected = id.filter(|i| self.find(i).is_some());
        for x in [before, self.selected.clone()].into_iter().flatten() {
            if let Some(e) = self.find(&x).cloned() {
                self.draw_entity(&e, h);
            }
        }
    }

    pub fn add(&mut self, e: Entity, h: &mut Host) -> bool {
        if self.entities.len() >= MAX_ENTITIES {
            return false;
        }
        self.selected = Some(e.id.clone());
        self.entities.push(e);
        self.draw_all(h);
        self.save(h);
        true
    }

    fn remember(&mut self, items: Vec<(Entity, usize)>, h: &Host) {
        self.undo = Some(Undo {
            items,
            until: h.now + UNDO_SECS,
        });
    }

    pub fn remove(&mut self, id: &str, h: &mut Host) {
        let Some(i) = self.entities.iter().position(|e| e.id == id) else {
            return;
        };
        let e = self.entities.remove(i);
        self.results.remove(id);
        self.undraw(id, h);
        if self.selected.as_deref() == Some(id) {
            self.selected = None;
        }
        self.save(h);
        self.remember(vec![(e, i)], h);
    }

    pub fn remove_all(&mut self, h: &mut Host) {
        if self.entities.is_empty() {
            return;
        }
        let items: Vec<(Entity, usize)> = std::mem::take(&mut self.entities)
            .into_iter()
            .enumerate()
            .map(|(i, e)| (e, i))
            .collect();
        for (e, _) in &items {
            self.undraw(&e.id, h);
        }
        self.results.clear();
        self.selected = None;
        self.save(h);
        self.remember(items, h);
    }

    pub fn undo_delete(&mut self, h: &mut Host) {
        let Some(u) = self.undo.take() else { return };
        let mut items = u.items;
        items.sort_by_key(|i| i.1);
        for (e, index) in items {
            self.selected = Some(e.id.clone());
            let at = index.min(self.entities.len());
            self.entities.insert(at, e);
        }
        self.draw_all(h);
        self.save(h);
    }

    /// Changes one entity and redraws and saves it.
    pub fn update(&mut self, id: &str, h: &mut Host, f: impl FnOnce(&mut Entity)) {
        if let Some(e) = self.find_mut(id) {
            f(e);
        }
        if let Some(e) = self.find(id).cloned() {
            self.draw_entity(&e, h);
        }
        self.save(h);
    }

    // ---- placing --------------------------------------------------------------

    pub fn stop_placing(&mut self, h: &mut Host) {
        if self.placing.take().is_some() {
            h.cancel_pick();
        }
        h.map().remove(PENDING_KEY);
    }

    pub fn start_placing(&mut self, kind: &'static str, h: &mut Host) {
        self.stop_placing(h);
        self.aim = None;
        self.placing = Some(Placing {
            kind,
            step: 0,
            first: None,
        });
        h.pick_point();
    }

    /// The arrow beside a coordinate or the direction: one click moves it.
    pub fn aim_at(&mut self, aim: Aim, h: &mut Host) {
        self.stop_placing(h);
        self.aim = Some(aim);
        h.pick_point();
    }

    fn picked(&mut self, at: [f64; 2], h: &mut Host) {
        if let Some(aim) = self.aim.take() {
            match aim {
                Aim::Post(id, which) => {
                    self.update(&id, h, |e| {
                        if which == 'b' {
                            e.lng2 = at[0];
                            e.lat2 = at[1];
                        } else {
                            e.lng = at[0];
                            e.lat = at[1];
                        }
                    });
                }
                Aim::Bearing(id) => {
                    if let Some(e) = self.find(&id) {
                        let b = (bearing_deg([e.lng, e.lat], at).round() as i64 % 360) as f64;
                        self.update(&id, h, |e| e.bearing = b);
                    }
                }
            }
            return;
        }
        let Some(p) = self.placing.clone() else { return };
        let kind = model::kind_of(p.kind).expect("a kind being placed exists");
        if kind.gate && p.step == 0 {
            self.placing = Some(Placing {
                step: 1,
                first: Some(at),
                ..p
            });
            if let Some(c) = shapes::hex(kind.color) {
                h.map().marker(
                    PENDING_KEY,
                    at,
                    "",
                    "",
                    "",
                    Style::color(c),
                    Some(18.0),
                    false,
                    false,
                    false,
                );
            }
            h.pick_point();
            return;
        }
        let e = make_entity(p.kind, p.first.unwrap_or(at), p.first.map(|_| at), &self.entities);
        h.map().remove(PENDING_KEY);
        self.placing = Some(Placing {
            step: 0,
            first: None,
            ..p
        });
        if let Some(e) = e {
            self.add(e, h);
        }
        h.pick_point();
    }

    /// The corners to frame everything on the map.
    pub fn all_points(&self) -> Vec<[f64; 2]> {
        self.entities.iter().flat_map(points_of).collect()
    }
}

/// The language files of this plugin, for the translation layer.
pub const LANG: &[(&str, &str)] = &[
    ("de", include_str!("../lang/de.json")),
    ("fr", include_str!("../lang/fr.json")),
    ("dl", include_str!("../lang/dl.json")),
];

/// One instance of the plugin, for the registry.
pub fn plugin() -> Box<dyn Plugin> {
    Box::new(MapEntities::default())
}

#[cfg(test)]
mod tests {

    #[test]
    fn the_plugin_says_what_the_web_plugins_manifest_says() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../python/plugins/map-entities/plugin.json"
        );
        let manifest: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let info = MapEntities::default().info();
        assert_eq!(manifest["id"], info.id);
        assert_eq!(manifest["name"], info.name);
        assert_eq!(manifest["icon"], info.icon);
        assert_eq!(manifest["description"], info.description);
        assert_eq!(manifest["version"], info.version);
        assert_eq!(manifest["order"], info.order);
    }

    #[test]
    fn the_language_files_hold_every_string_of_the_web_plugins_files() {
        for (code, ours) in LANG {
            let path = format!(
                concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/../../../python/plugins/map-entities/lang/{}.json"
                ),
                code
            );
            let web = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
            let (ours, web): (Value, Value) =
                (serde_json::from_str(ours).unwrap(), serde_json::from_str(&web).unwrap());
            for (k, v) in web["strings"].as_object().unwrap() {
                assert_eq!(&ours["strings"][k], v, "{code}: {k}");
            }
        }
    }
    use super::*;

    #[test]
    fn a_shapes_key_names_its_entity() {
        assert_eq!(entity_of_key("e1abc-m"), Some("e1abc".into()));
        assert_eq!(entity_of_key("my-id_1-a"), Some("my-id_1".into()));
        assert_eq!(entity_of_key("pending"), None);
        assert_eq!(entity_of_key("x-z"), None);
    }

    #[test]
    fn the_options_default_to_on() {
        let o = Opts::default();
        assert!(o.labels && o.notify && o.armed_only);
    }
}
