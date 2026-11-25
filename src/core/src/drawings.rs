//! What an AI client draws on the Home map: lines, arrows, areas, circles,
//! sectors, markers and text, kept here as data and painted by every window.
//!
//! The list lives in memory and ends with the program. It is a sketch for the
//! operator: nothing of it is sent to the vehicle, and nothing flies by it
//! unless a tool is told to. The `drawings` topic carries the whole list on
//! every change (it is bounded, see the caps), a `show` event that asks the
//! windows to frame some places, and an `open` event that asks them to open a
//! page ([`PAGES`]).

use std::sync::{Arc, Mutex, MutexGuard};

use serde_json::{json, Value};

use crate::core::Core;
use crate::events::{Binding, Topic};
use crate::json::Map;
use crate::mcp::geo;
use crate::module::{Module, Registry};
use crate::request::Request;
use crate::response::Response;

/// The pages a window can be asked to open, as `--page` names them, and the tabs of the side
/// panel. A plugin's own view is `plugin:<id>` besides. The interface holds its pages to this list.
pub const PAGES: [&str; 41] = [
    "home",
    "mission",
    "setup",
    "setup:firstsetup",
    "setup:calibration",
    "setup:control",
    "setup:tuning",
    "setup:motors",
    "setup:safety",
    "setup:battery",
    "setup:sik",
    "setup:rtk",
    "setup:remoteid",
    "setup:parameters",
    "setup:firmware",
    "setup:betaflight",
    "setup:video",
    "analysis",
    "analysis:ulog",
    "analysis:tlog",
    "analysis:review",
    "analysis:scope",
    "logs",
    "settings",
    "settings:interface",
    "settings:layout",
    "settings:flight",
    "settings:map",
    "settings:controls",
    "settings:connections",
    "settings:data",
    "settings:plugins",
    "settings:about",
    "panel:link",
    "panel:console",
    "panel:ssh",
    "panel:plugins",
    "notifications",
    "map:2d",
    "map:3d",
    "map:vehicle",
];

/// Whether a window can be asked to open `page`.
pub fn is_page(page: &str) -> bool {
    PAGES.contains(&page)
        || page
            .strip_prefix("plugin:")
            .is_some_and(crate::plugins::registry::is_valid_id)
}

pub const MAX_SHAPES: usize = 200;
/// The most points one shape may have.
pub const MAX_POINTS: usize = 500;
/// The most points all shapes may have together.
pub const MAX_TOTAL: usize = 10_000;
pub const TEXT_MAX: usize = 200;
/// The largest circle or sector, in metres.
pub const RADIUS_MAX_M: f64 = 100_000.0;
const TOPIC_CAPACITY: usize = 32;
const KINDS: [&str; 7] = ["line", "arrow", "polygon", "circle", "sector", "marker", "text"];
const COLORS: [(&str, &str); 11] = [
    ("cyan", "#2BC4E4"),
    ("lime", "#7BD389"),
    ("green", "#1FA971"),
    ("magenta", "#C77DFF"),
    ("blue", "#5B8DEF"),
    ("white", "#FFFFFF"),
    ("red", "#FF5A52"),
    ("orange", "#FF9F0A"),
    ("yellow", "#FFD60A"),
    ("black", "#101418"),
    ("gray", "#A0A7B4"),
];

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

type Listener = Arc<dyn Fn(&Value) + Send + Sync>;

#[derive(Default)]
struct State {
    shapes: Vec<Map>,
    next: u64,
    listeners: Vec<(u64, Listener)>,
    next_listener: u64,
}

impl State {
    fn tell(&self, event: &Value) {
        for (_, f) in &self.listeners {
            f(event);
        }
    }

    fn snapshot(&self) -> Value {
        json!({"type": "snapshot", "shapes": self.shapes})
    }
}

/// The drawings of this run.
#[derive(Default)]
pub struct DrawingsService {
    state: Mutex<State>,
}

fn finite(v: Option<&Value>) -> Option<f64> {
    v.and_then(Value::as_f64).filter(|n| n.is_finite())
}

fn position(v: &Value) -> Option<[f64; 2]> {
    let (lat, lon) = (finite(v.get("lat"))?, finite(v.get("lon"))?);
    (lat.abs() <= 90.0 && lon.abs() <= 180.0).then_some([lat, lon])
}

fn color(v: Option<&Value>) -> Result<String, String> {
    let Some(v) = v.filter(|v| !v.is_null()) else {
        return Ok(COLORS[0].1.into());
    };
    let text = v.as_str().map(str::trim).unwrap_or("").to_lowercase();
    if let Some((_, hex)) = COLORS.iter().find(|(n, _)| *n == text) {
        return Ok((*hex).into());
    }
    match text.strip_prefix('#') {
        Some(h) if h.len() == 6 && h.bytes().all(|b| b.is_ascii_hexdigit()) => Ok(format!("#{}", h.to_uppercase())),
        _ => Err(format!(
            "color must be #RRGGBB or one of {}",
            COLORS.map(|(n, _)| n).join(", ")
        )),
    }
}

fn plain(v: Option<&Value>) -> String {
    let text = v.and_then(Value::as_str).unwrap_or("");
    let one_line: Vec<&str> = text
        .split(|c: char| c.is_whitespace() || c.is_control())
        .filter(|p| !p.is_empty())
        .collect();
    one_line.join(" ").chars().take(TEXT_MAX).collect()
}

fn valid_id(id: &str) -> bool {
    (1..=40).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// One shape with only what is known and in range, or why it cannot be drawn.
/// Its `id` is empty when it named none.
pub fn clean(raw: &Value) -> Result<Map, String> {
    let Some(raw) = raw.as_object() else {
        return Err("a shape must be an object".into());
    };
    let kind = raw.get("kind").and_then(Value::as_str).unwrap_or("");
    if !KINDS.contains(&kind) {
        return Err(format!("kind must be one of {}", KINDS.join(", ")));
    }
    let id = match raw.get("id") {
        None | Some(Value::Null) => "",
        Some(Value::String(s)) if valid_id(s) => s,
        Some(_) => return Err("id must be 1 to 40 letters, digits, _ or -".into()),
    };
    let mut out = Map::new();
    out.insert("id".into(), id.into());
    out.insert("kind".into(), kind.into());
    let place = |out: &mut Map| -> Result<(), String> {
        let p = position(&Value::Object(raw.clone())).ok_or(format!("a {kind} needs lat and lon"))?;
        out.insert("lat".into(), json!(p[0]));
        out.insert("lon".into(), json!(p[1]));
        Ok(())
    };
    match kind {
        "line" | "arrow" | "polygon" => {
            let list = raw
                .get("points")
                .and_then(Value::as_array)
                .ok_or(format!("a {kind} needs points"))?;
            let least = if kind == "polygon" { 3 } else { 2 };
            if !(least..=MAX_POINTS).contains(&list.len()) {
                return Err(format!("a {kind} needs {least} to {MAX_POINTS} points"));
            }
            let points: Option<Vec<[f64; 2]>> = list.iter().map(position).collect();
            let points = points.ok_or("every point needs a lat and a lon on the globe")?;
            let points: Vec<Value> = points.iter().map(|p| json!({"lat": p[0], "lon": p[1]})).collect();
            out.insert("points".into(), Value::Array(points));
        }
        _ => place(&mut out)?,
    }
    if matches!(kind, "circle" | "sector") {
        let r = finite(raw.get("radius_m")).filter(|r| *r > 0.0 && *r <= RADIUS_MAX_M);
        out.insert(
            "radius_m".into(),
            json!(r.ok_or(format!("radius_m must be over 0 and at most {RADIUS_MAX_M}"))?),
        );
    }
    if kind == "sector" {
        for key in ["from_deg", "to_deg"] {
            let v = finite(raw.get(key)).ok_or(format!("a sector needs {key}"))?;
            out.insert(key.into(), json!(v.rem_euclid(360.0)));
        }
    }
    let text = plain(raw.get("text"));
    if kind == "text" && text.is_empty() {
        return Err("a text needs text".into());
    }
    if !text.is_empty() {
        out.insert("text".into(), text.into());
    }
    let icon = plain(raw.get("icon"));
    if kind == "marker" && !icon.is_empty() {
        out.insert("icon".into(), icon.to_lowercase().into());
    }
    out.insert("color".into(), color(raw.get("color"))?.into());
    for (key, low, high) in [("width", 1.0, 12.0), ("fill_opacity", 0.0, 1.0), ("size", 9.0, 48.0)] {
        match raw.get(key) {
            None | Some(Value::Null) => {}
            v => {
                let n = finite(v).ok_or(format!("{key} must be a number"))?;
                out.insert(key.into(), json!(n.clamp(low, high)));
            }
        }
    }
    if raw.get("dashed") == Some(&Value::Bool(true)) {
        out.insert("dashed".into(), true.into());
    }
    Ok(out)
}

/// The places a shape covers, as `[lat, lon]`: its points, or the rim and the
/// centre of a round one.
pub fn outline(shape: &Map) -> Vec<[f64; 2]> {
    if let Some(points) = shape.get("points").and_then(Value::as_array) {
        return points.iter().filter_map(position).collect();
    }
    let Some(at) = position(&Value::Object(shape.clone())) else {
        return Vec::new();
    };
    match (shape.get("kind").and_then(Value::as_str), finite(shape.get("radius_m"))) {
        (Some("circle"), Some(r)) => geo::ring(at, r, 48, 0.0, true),
        (Some("sector"), Some(r)) => {
            let (from, to) = (finite(shape.get("from_deg")), finite(shape.get("to_deg")));
            geo::sector(at, r, from.unwrap_or(0.0), to.unwrap_or(0.0))
        }
        _ => vec![at],
    }
}

fn points_of(shape: &Map) -> usize {
    shape.get("points").and_then(Value::as_array).map_or(1, Vec::len)
}

impl DrawingsService {
    pub fn list(&self) -> Vec<Map> {
        lock(&self.state).shapes.clone()
    }

    /// Draws `shapes`; one whose id is already there replaces it. With `clear`
    /// everything else goes. Nothing is drawn when one of them is refused.
    pub fn draw(&self, shapes: &[Value], clear: bool) -> Result<Vec<String>, String> {
        if shapes.is_empty() {
            return Err("shapes must be a non-empty list".into());
        }
        if shapes.len() > MAX_SHAPES {
            return Err(format!("at most {MAX_SHAPES} shapes fit on the map"));
        }
        let mut cleaned = Vec::new();
        for (i, raw) in shapes.iter().enumerate() {
            cleaned.push(clean(raw).map_err(|why| format!("shape {i}: {why}"))?);
        }
        let mut st = lock(&self.state);
        let mut next = if clear { Vec::new() } else { st.shapes.clone() };
        let mut counter = st.next;
        let mut ids = Vec::new();
        for mut shape in cleaned {
            let mut id = shape["id"].as_str().unwrap_or("").to_string();
            while id.is_empty() || (shape["id"] == "" && next.iter().any(|s| s["id"] == json!(id))) {
                counter += 1;
                id = format!("d{counter}");
            }
            shape.insert("id".into(), id.clone().into());
            match next.iter_mut().find(|s| s["id"] == json!(id)) {
                Some(slot) => *slot = shape,
                None => next.push(shape),
            }
            ids.push(id);
        }
        if next.len() > MAX_SHAPES {
            return Err(format!("at most {MAX_SHAPES} shapes fit on the map, remove some first"));
        }
        if next.iter().map(points_of).sum::<usize>() > MAX_TOTAL {
            return Err(format!("at most {MAX_TOTAL} points fit on the map, remove some first"));
        }
        st.shapes = next;
        st.next = counter;
        st.tell(&st.snapshot());
        Ok(ids)
    }

    /// Takes the named shapes away, or all of them: how many went.
    pub fn remove(&self, ids: Option<&[String]>) -> usize {
        let mut st = lock(&self.state);
        let before = st.shapes.len();
        st.shapes
            .retain(|s| ids.is_some_and(|ids| !ids.iter().any(|id| s["id"] == json!(id))));
        let gone = before - st.shapes.len();
        if gone > 0 {
            st.tell(&st.snapshot());
        }
        gone
    }

    /// Asks the windows to frame these places, `[lat, lon]`, one place at `zoom`, with the Home
    /// page brought to the front when `open` says so.
    pub fn show(&self, places: &[[f64; 2]], zoom: Option<f64>, open: bool) {
        let points: Vec<[f64; 2]> = places.iter().map(|p| [p[1], p[0]]).collect();
        let mut event = json!({"type": "show", "points": points});
        if let Some(z) = zoom {
            event["zoom"] = json!(z);
        }
        if open {
            event["open"] = json!(true);
        }
        lock(&self.state).tell(&event);
    }

    /// Asks the windows to open a page of [`PAGES`].
    pub fn open(&self, page: &str) {
        lock(&self.state).tell(&json!({"type": "open", "page": page}));
    }

    fn add_listener(&self, f: Listener) -> (u64, Value) {
        let mut st = lock(&self.state);
        st.next_listener += 1;
        let id = st.next_listener;
        st.listeners.push((id, f));
        (id, st.snapshot())
    }

    fn remove_listener(&self, id: u64) {
        lock(&self.state).listeners.retain(|(i, _)| *i != id);
    }
}

#[derive(Default)]
pub struct DrawingsModule {
    service: Arc<DrawingsService>,
}

impl DrawingsModule {
    pub fn new() -> DrawingsModule {
        DrawingsModule::default()
    }
}

impl Module for DrawingsModule {
    fn name(&self) -> &'static str {
        "drawings"
    }

    fn register(&self, reg: &mut Registry) {
        reg.service(Arc::clone(&self.service));
        reg.topic(Topic::new("drawings", TOPIC_CAPACITY, |core, sink, _job| {
            let Some(svc) = core.service::<DrawingsService>() else {
                return Binding::none();
            };
            let (id, first) = svc.add_listener(Arc::new(move |event: &Value| sink.put_fifo(event.clone())));
            let svc2 = svc.clone();
            Binding {
                unbind: Box::new(move || svc2.remove_listener(id)),
                initial: Some(first),
            }
        }));
        reg.get("/api/map/drawings", api_list);
        reg.post("/api/map/drawings", api_draw);
        reg.post("/api/map/drawings/remove", api_remove);
        reg.post("/api/map/show", api_show);
        reg.post("/api/window/open", api_open);
    }
}

// ---------------------------------------------------------------- routes

macro_rules! svc {
    ($core:expr) => {
        match $core.service::<DrawingsService>() {
            Some(s) => s,
            None => return Response::not_ok(503, "The map drawings are not available"),
        }
    };
}

fn api_list(core: &Core, _req: &Request) -> Response {
    let list = svc!(core).list();
    Response::json(json!({"count": list.len(), "shapes": list}))
}

fn api_draw(core: &Core, req: &Request) -> Response {
    let svc = svc!(core);
    let p = req.payload();
    let Some(shapes) = p.get("shapes").and_then(Value::as_array) else {
        return Response::not_ok(400, "shapes must be a non-empty list");
    };
    let clear = match p.get("clear") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Response::not_ok(400, "clear must be true or false"),
    };
    match svc.draw(shapes, clear) {
        Ok(ids) => Response::json(json!({"ok": true, "ids": ids})),
        Err(why) => Response::not_ok(400, &why),
    }
}

fn ids_of(v: Option<&Value>) -> Result<Option<Vec<String>>, String> {
    match v {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(a)) if a.iter().all(Value::is_string) => {
            Ok(Some(a.iter().filter_map(Value::as_str).map(str::to_string).collect()))
        }
        Some(_) => Err("ids must be a list of drawing ids".into()),
    }
}

fn api_remove(core: &Core, req: &Request) -> Response {
    let svc = svc!(core);
    let p = req.payload();
    let ids = match ids_of(p.get("ids")) {
        Ok(ids) => ids,
        Err(why) => return Response::not_ok(400, &why),
    };
    let all = p.get("all") == Some(&Value::Bool(true));
    if ids.is_none() && !all {
        return Response::not_ok(400, "name the ids, or say all");
    }
    let removed = svc.remove(if all { None } else { ids.as_deref() });
    Response::json(json!({"ok": true, "removed": removed}))
}

fn api_show(core: &Core, req: &Request) -> Response {
    let svc = svc!(core);
    let p = req.payload();
    let ids = match ids_of(p.get("ids")) {
        Ok(ids) => ids.unwrap_or_default(),
        Err(why) => return Response::not_ok(400, &why),
    };
    let mut places: Vec<[f64; 2]> = Vec::new();
    for raw in p.get("points").and_then(Value::as_array).into_iter().flatten() {
        match position(raw) {
            Some(at) if places.len() < MAX_POINTS => places.push(at),
            Some(_) => return Response::not_ok(400, &format!("at most {MAX_POINTS} points")),
            None => return Response::not_ok(400, "every point needs a lat and a lon on the globe"),
        }
    }
    let drawn = svc.list();
    for id in &ids {
        match drawn.iter().find(|s| s["id"] == json!(id)) {
            Some(shape) => places.extend(outline(shape)),
            None => return Response::not_ok(404, &format!("no drawing called {id}")),
        }
    }
    if places.is_empty() {
        return Response::not_ok(400, "name points or ids to show");
    }
    let zoom = match p.get("zoom") {
        None | Some(Value::Null) => None,
        Some(z) => match z.as_f64().filter(|z| (1.0..=20.0).contains(z)) {
            Some(z) => Some(z),
            None => return Response::not_ok(400, "zoom must be a number from 1 to 20"),
        },
    };
    svc.show(&places, zoom, p.get("open") == Some(&json!(true)));
    Response::json(json!({"ok": true}))
}

fn api_open(core: &Core, req: &Request) -> Response {
    let svc = svc!(core);
    match req.payload().get("page").and_then(Value::as_str) {
        Some(page) if is_page(page) => {
            svc.open(page);
            Response::json(json!({"ok": true}))
        }
        _ => Response::not_ok(400, "there is no page of that name"),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn pages_are_the_known_ones_and_the_plugins() {
        assert!(is_page("settings:plugins") && is_page("plugin:map-entities"));
        for bad in [
            "",
            "Settings",
            "settings:",
            "setup:nope",
            "plugin:",
            "plugin:../x",
            "plugin:a b",
        ] {
            assert!(!is_page(bad), "{bad}");
        }
        let mut all = PAGES.to_vec();
        all.sort_unstable();
        all.dedup();
        assert_eq!(all.len(), PAGES.len());
    }

    use super::*;

    fn line(id: &str) -> Value {
        json!({"kind": "line", "id": id, "points": [{"lat": 48.0, "lon": 11.0}, {"lat": 48.1, "lon": 11.1}]})
    }

    #[test]
    fn a_shape_keeps_only_what_is_known_and_in_range() {
        let s = clean(
            &json!({"kind": "circle", "lat": 48.0, "lon": 11.0, "radius_m": 50, "color": "red",
                              "width": 99, "evil": "x", "text": "a\n  b\u{0007}c"}),
        )
        .unwrap();
        assert_eq!(s["color"], "#FF5A52");
        assert_eq!(s["width"], 12.0);
        assert_eq!(s["text"], "a b c");
        assert!(!s.contains_key("evil"));
        assert_eq!(
            clean(&json!({"kind": "marker", "lat": 1, "lon": 2, "color": "#aabbcc"})).unwrap()["color"],
            "#AABBCC"
        );
    }

    #[test]
    fn a_shape_that_cannot_be_drawn_is_refused_with_the_reason() {
        for (bad, why) in [
            (json!(3), "object"),
            (json!({"kind": "blob"}), "kind must be one of"),
            (json!({"kind": "line", "points": [{"lat": 1, "lon": 1}]}), "2 to 500"),
            (
                json!({"kind": "line", "points": [{"lat": 1, "lon": 1}, {"lat": 91, "lon": 1}]}),
                "on the globe",
            ),
            (
                json!({"kind": "polygon", "points": [{"lat": 1, "lon": 1}, {"lat": 2, "lon": 1}]}),
                "3 to 500",
            ),
            (json!({"kind": "circle", "lat": 1, "lon": 1}), "radius_m"),
            (
                json!({"kind": "circle", "lat": 1, "lon": 1, "radius_m": -4}),
                "radius_m",
            ),
            (json!({"kind": "sector", "lat": 1, "lon": 1, "radius_m": 4}), "from_deg"),
            (json!({"kind": "text", "lat": 1, "lon": 1, "text": " "}), "needs text"),
            (json!({"kind": "marker", "lat": 1}), "lat and lon"),
            (json!({"kind": "marker", "lat": 1, "lon": 1, "color": "beige"}), "color"),
            (json!({"kind": "marker", "lat": 1, "lon": 1, "id": "a b"}), "id must"),
        ] {
            let e = clean(&bad).unwrap_err();
            assert!(e.contains(why), "{bad}: {e}");
        }
    }

    #[test]
    fn an_id_drawn_again_moves_the_shape_and_a_bad_one_draws_nothing() {
        let svc = DrawingsService::default();
        assert_eq!(svc.draw(&[line("route")], false).unwrap(), ["route"]);
        let unnamed = json!({"kind": "marker", "lat": 1, "lon": 2});
        assert_eq!(svc.draw(&[unnamed.clone(), unnamed], false).unwrap(), ["d1", "d2"]);
        svc.draw(&[line("route")], false).unwrap();
        assert_eq!(svc.list().len(), 3);
        assert!(svc.draw(&[line("x"), json!({})], false).is_err());
        assert_eq!(svc.list().len(), 3, "all of them or none");
        assert_eq!(svc.remove(Some(&["d1".into(), "nope".into()])), 1);
        svc.draw(&[line("new")], true).unwrap();
        assert_eq!(svc.list().len(), 1, "clear leaves only the new ones");
        assert_eq!(svc.remove(None), 1);
    }

    #[test]
    fn the_map_never_holds_more_than_its_caps() {
        let svc = DrawingsService::default();
        let marker = json!({"kind": "marker", "lat": 1, "lon": 2});
        svc.draw(&vec![marker.clone(); MAX_SHAPES], false).unwrap();
        assert!(svc.draw(&[marker], false).unwrap_err().contains("at most"));
        svc.remove(None);
        let points: Vec<Value> = (0..MAX_POINTS)
            .map(|i| json!({"lat": 1.0, "lon": i as f64 / 10.0}))
            .collect();
        let long = json!({"kind": "line", "points": points});
        svc.draw(&vec![long.clone(); MAX_TOTAL / MAX_POINTS], false).unwrap();
        assert!(svc.draw(&[long], false).unwrap_err().contains("points"));
    }

    #[test]
    fn a_window_hears_the_list_and_what_to_show() {
        let svc = DrawingsService::default();
        let heard: Arc<Mutex<Vec<Value>>> = Arc::default();
        let h = heard.clone();
        let (id, first) = svc.add_listener(Arc::new(move |e: &Value| lock(&h).push(e.clone())));
        assert_eq!(first, json!({"type": "snapshot", "shapes": []}));
        svc.draw(&[line("a")], false).unwrap();
        svc.show(&[[48.0, 11.0]], None, false);
        svc.show(&[[48.0, 11.0]], Some(9.0), true);
        svc.open("settings:map");
        svc.remove_listener(id);
        svc.remove(None);
        let heard = lock(&heard);
        assert_eq!(heard.len(), 4);
        assert_eq!(heard[0]["shapes"][0]["id"], "a");
        assert_eq!(heard[1], json!({"type": "show", "points": [[11.0, 48.0]]}));
        assert_eq!(
            heard[2],
            json!({"type": "show", "points": [[11.0, 48.0]], "zoom": 9.0, "open": true})
        );
        assert_eq!(heard[3], json!({"type": "open", "page": "settings:map"}));
    }
}
