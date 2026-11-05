//! What the Settings page knows without drawing anything: the pills, the
//! choices of each picker, the shape of a config write, and the sentences the
//! cards build from the backend's answers. Each rule has the web interface's
//! expected text in a test (`python/src/js/sidenav.js`, `settings-mcp.js`).

use serde_json::{json, Value};

use crate::data::units::Units;

/// The pills, in the order of `SETTINGS_TABS`.
pub const TABS: [(&str, &str, &str); 9] = [
    ("interface", "Interface", "palette"),
    ("layout", "Layout", "panels-top-left"),
    ("flight", "Flight view", "gauge"),
    ("map", "Map", "map"),
    ("controls", "Controls", "gamepad-2"),
    ("connections", "Connections", "plug"),
    ("data", "Data", "database"),
    ("plugins", "Plugins", "puzzle"),
    ("about", "About", "info"),
];

/// `Corvus.theme.THEMES`: id, label, description, swatch (accent, panel, page).
pub const THEMES: [(&str, &str, &str, [u32; 3]); 6] = [
    ("light", "Light", "Default", [0x1B1F26, 0xF1F3F6, 0xFFFFFF]),
    (
        "light-orange",
        "Light Orange",
        "Corporate Orange",
        [0xC2540A, 0xFFFFFF, 0xF4F6F8],
    ),
    ("green", "Green", "Dark", [0x3DA876, 0x171D25, 0x0B0E12]),
    ("blue", "Blue", "Cool", [0x3B9EFF, 0x171D25, 0x0B0E12]),
    ("pink", "Pink", "Magenta", [0xF0509B, 0x171D25, 0x0B0E12]),
    ("orange", "Orange", "Amber", [0xF58A2B, 0x171D25, 0x0B0E12]),
];

/// `Corvus.scale.STEPS`.
pub const SCALE_STEPS: [(f32, &str); 6] = [
    (0.8, "80%"),
    (0.9, "90%"),
    (1.0, "100%"),
    (1.1, "110%"),
    (1.25, "125%"),
    (1.5, "150%"),
];

/// `Corvus.joystick.GAIN_STEPS`.
pub const GAIN_STEPS: [(f32, &str); 4] = [(0.25, "25%"), (0.5, "50%"), (0.75, "75%"), (1.0, "100%")];

/// The step a stored number sits nearest to, as the slider shows it.
pub fn nearest_step(steps: &[(f32, &str)], value: f32) -> usize {
    steps
        .iter()
        .enumerate()
        .min_by(|a, b| {
            (a.1 .0 - value)
                .abs()
                .partial_cmp(&(b.1 .0 - value).abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map_or(0, |(i, _)| i)
}

/// The interface size the config asks for (`Corvus.scale.fromConfig`): clamped
/// into the range on offer, never snapped to a step.
pub fn scale_from_config(cfg: &Value) -> f32 {
    let n = cfg
        .get("ui")
        .and_then(|u| u.get("scale"))
        .and_then(Value::as_f64)
        .unwrap_or(1.0) as f32;
    if !n.is_finite() || n <= 0.0 {
        1.0
    } else {
        n.clamp(SCALE_STEPS[0].0, SCALE_STEPS[SCALE_STEPS.len() - 1].0)
    }
}

/// `Corvus.units.SYSTEMS`: id, label, description, and the four unit ids.
pub const UNIT_SYSTEMS: [(&str, &str, &str, [&str; 4]); 3] = [
    ("metric", "Metric", "m, km, m/s, \u{b0}C", ["m", "km", "ms", "c"]),
    ("imperial", "Imperial", "ft, mi, mph, \u{b0}F", ["ft", "mi", "mph", "f"]),
    ("aviation", "Aviation", "ft, NM, kn, \u{b0}C", ["ft", "nmi", "kn", "c"]),
];

/// The four quantities in the order the card lists them: key, caption, hint.
pub const UNIT_ROWS: [(&str, &str, &str); 4] = [
    (
        "length",
        "Altitude and length",
        "Altitudes, heights, radii and GPS accuracy.",
    ),
    (
        "distance",
        "Distance",
        "Route lengths and distances to a place. Short hops stay in the length unit.",
    ),
    ("speed", "Speed", "Ground speed, vertical speed and mission speeds."),
    ("temperature", "Temperature", "Battery temperature."),
];

/// The units of one quantity as the selects list them.
pub fn unit_choices(quantity: &str) -> &'static [(&'static str, &'static str)] {
    match quantity {
        "length" => &[("m", "Metres (m)"), ("ft", "Feet (ft)")],
        "distance" => &[
            ("km", "Kilometres (km)"),
            ("mi", "Miles (mi)"),
            ("nmi", "Nautical miles (NM)"),
        ],
        "speed" => &[
            ("ms", "Metres per second (m/s)"),
            ("kmh", "Kilometres per hour (km/h)"),
            ("mph", "Miles per hour (mph)"),
            ("kn", "Knots (kn)"),
        ],
        _ => &[("c", "Celsius (\u{b0}C)"), ("f", "Fahrenheit (\u{b0}F)")],
    }
}

/// The four stored unit ids of a config, an unknown or missing one falling
/// back to metric (`Corvus.units.normalize`).
pub fn units_of(cfg: &Value) -> [String; 4] {
    let raw = cfg.get("ui").and_then(|u| u.get("units"));
    let mut out = [String::new(), String::new(), String::new(), String::new()];
    for (i, (q, _, _)) in UNIT_ROWS.iter().enumerate() {
        let want = raw.and_then(|r| r.get(*q)).and_then(Value::as_str).unwrap_or("");
        let choices = unit_choices(q);
        out[i] = if choices.iter().any(|c| c.0 == want) {
            want.to_string()
        } else {
            choices[0].0.to_string()
        };
    }
    out
}

/// The preset the four units match, if any (`Corvus.units.systemOf`).
pub fn system_of(units: &[String; 4]) -> Option<&'static str> {
    UNIT_SYSTEMS
        .iter()
        .find(|s| s.3.iter().zip(units).all(|(a, b)| a == b))
        .map(|s| s.0)
}

/// The `ui.units` object, all four keys, as every write posts it (the backend
/// merges `ui` one level deep and would drop the other three).
pub fn units_value(units: &[String; 4]) -> Value {
    let mut o = serde_json::Map::new();
    for (i, (q, _, _)) in UNIT_ROWS.iter().enumerate() {
        o.insert((*q).into(), Value::String(units[i].clone()));
    }
    Value::Object(o)
}

/// The same units the rest of the app formats numbers in.
pub fn units_in_force(cfg: &Value) -> Units {
    Units::from_config(cfg)
}

/// Review sensitivity (`REVIEW_SENSITIVITIES`).
pub const REVIEW: [(&str, &str, &str); 3] = [
    (
        "relaxed",
        "Relaxed",
        "Only what should keep the aircraft on the ground.",
    ),
    (
        "normal",
        "Normal",
        "A healthy aircraft flown normally shows no findings.",
    ),
    ("strict", "Strict", "Every early sign, for tuning and new airframes."),
];

/// The saved sensitivity, "normal" when it is none of the three.
pub fn review_of(cfg: &Value) -> &'static str {
    let saved = cfg
        .get("review")
        .and_then(|r| r.get("sensitivity"))
        .and_then(Value::as_str)
        .unwrap_or("");
    REVIEW.iter().find(|r| r.0 == saved).map_or("normal", |r| r.0)
}

/// The mapped value of `section.key`, if the config holds one.
pub fn get<'a>(cfg: &'a Value, section: &str, key: &str) -> Option<&'a Value> {
    cfg.get(section).and_then(|s| s.get(key))
}

/// A boolean setting; `default` when it is absent or not a boolean.
pub fn flag(cfg: &Value, section: &str, key: &str, default: bool) -> bool {
    get(cfg, section, key).and_then(Value::as_bool).unwrap_or(default)
}

/// A text setting, empty when absent.
pub fn text<'a>(cfg: &'a Value, section: &str, key: &str) -> &'a str {
    get(cfg, section, key).and_then(Value::as_str).unwrap_or("")
}

/// `{section: {key: value}}`, the body of `POST /api/config`.
pub fn patch(section: &str, key: &str, value: Value) -> Value {
    json!({ section: { key: value } })
}

/// Lays `patch` over `cfg` the way the backend merges a write: objects merge
/// key by key, anything else replaces. Returns the previous values of exactly
/// the keys written, as a patch that undoes it (`null` for a key that was not
/// there, which merges as "set to null"; the backend ignores that on read).
pub fn merge(cfg: &mut Value, patch: &Value) -> Value {
    let (Some(target), Some(src)) = (cfg.as_object_mut(), patch.as_object()) else {
        return Value::Null;
    };
    let mut undo = serde_json::Map::new();
    for (k, v) in src {
        match (target.get_mut(k), v) {
            (Some(existing), Value::Object(_)) if existing.is_object() => {
                undo.insert(k.clone(), merge(existing, v));
            }
            (existing, _) => {
                undo.insert(k.clone(), existing.map_or(Value::Null, |e| e.clone()));
                target.insert(k.clone(), v.clone());
            }
        }
    }
    Value::Object(undo)
}

/// `Corvus.ui.optionCards` for the top bar's altitude.
pub const ALTITUDE_REFS: [(&str, &str, &str); 2] = [
    ("amsl", "AMSL", "Above mean sea level"),
    ("relative", "Relative", "Above home"),
];

/// The altitude reference the config names, AMSL when it is neither.
pub fn altitude_ref(cfg: &Value) -> &'static str {
    match text(cfg, "ui", "topbar_altitude") {
        "relative" => "relative",
        _ => "amsl",
    }
}

/// The map animation rate (`Corvus.anim.setMapRate`): 30 or 60.
pub fn map_rate(cfg: &Value) -> u32 {
    match get(cfg, "ui", "map_animation_rate").and_then(Value::as_f64) {
        Some(r) if (r - 30.0).abs() < 0.5 => 30,
        _ => 60,
    }
}

/// One-line summary of what a map service offers (`describeProvider`).
pub fn describe_provider(prov: &Value, sources: &[Value]) -> String {
    let ids: Vec<&str> = prov
        .get("sources")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let flag = |k: &str| prov.get(k).and_then(Value::as_bool).unwrap_or(false);
    if flag("token_required") && !flag("token_set") {
        return "Needs an API key".into();
    }
    let cached = ids
        .iter()
        .filter(|id| {
            sources
                .iter()
                .find(|s| s.get("id").and_then(Value::as_str) == Some(**id))
                .and_then(|s| s.get("cached_count").and_then(Value::as_f64))
                .unwrap_or(0.0)
                > 0.0
        })
        .count();
    let mut parts = vec![if ids.len() == 1 {
        "1 layer".to_string()
    } else {
        format!("{} layers", ids.len())
    }];
    if cached > 0 {
        parts.push(format!("{cached} cached"));
    }
    if flag("token_optional") && flag("token_set") {
        parts.push("licensed API".into());
    }
    parts.join(" \u{b7} ")
}

/// `Number.prototype.toLocaleString()` for a count, in the browser's default
/// grouping (comma, as the English locale writes it).
pub fn grouped(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// The cache note under the layer select (`describeCache`).
pub fn describe_cache(source: &Value) -> String {
    let n = source.get("cached_count").and_then(Value::as_f64).unwrap_or(0.0) as u64;
    let maxzoom = source.get("maxzoom").and_then(Value::as_i64).unwrap_or(0);
    if n > 0 {
        format!("{} tiles cached offline \u{b7} up to z{maxzoom}", grouped(n))
    } else {
        format!("Nothing cached yet \u{b7} up to z{maxzoom}")
    }
}

/// "A, B and C" (`listNames`).
pub fn list_names(list: &[String]) -> String {
    match list.len() {
        0 => String::new(),
        1 => list[0].clone(),
        n => format!("{} and {}", list[..n - 1].join(", "), list[n - 1]),
    }
}

/// The note under API KEYS: which services wait for a key.
pub fn keys_note(keyed: &[Value]) -> String {
    let label = |p: &Value| p.get("label").and_then(Value::as_str).unwrap_or("").to_string();
    let flag = |p: &Value, k: &str| p.get(k).and_then(Value::as_bool).unwrap_or(false);
    let unset: Vec<&Value> = keyed.iter().filter(|p| !flag(p, "token_set")).collect();
    let hidden: Vec<String> = unset
        .iter()
        .filter(|p| flag(p, "token_required"))
        .map(|p| label(p))
        .collect();
    let optional: Vec<String> = unset
        .iter()
        .filter(|p| !flag(p, "token_required"))
        .map(|p| label(p))
        .collect();
    let mut lines = Vec::new();
    if !hidden.is_empty() {
        lines.push(format!(
            "{} {} as a map service once you add an API key.",
            list_names(&hidden),
            if hidden.len() == 1 { "appears" } else { "appear" }
        ));
    }
    if !optional.is_empty() {
        lines.push(format!(
            "{} {} licensed API once you add a key.",
            list_names(&optional),
            if optional.len() == 1 { "uses its" } else { "use their" }
        ));
    }
    if lines.is_empty() {
        "Every map service has a key.".into()
    } else {
        lines.join(" ")
    }
}

/// The map service the page shows as chosen: the saved base layer's provider,
/// else the saved provider, else the backend's default, else the first.
pub fn active_provider(cfg: &Value, providers: &[Value], sources: &[Value], fallback: &str) -> String {
    let saved = text(cfg, "map", "base_layer");
    let by_layer = sources
        .iter()
        .find(|s| s.get("id").and_then(Value::as_str) == Some(saved))
        .and_then(|s| s.get("provider").and_then(Value::as_str));
    if let Some(p) = by_layer {
        return p.into();
    }
    let provider = text(cfg, "map", "provider");
    if !provider.is_empty() {
        return provider.into();
    }
    if !fallback.is_empty() {
        return fallback.into();
    }
    providers
        .first()
        .and_then(|p| p.get("id").and_then(Value::as_str))
        .unwrap_or("")
        .into()
}

/// The services that get a card: a keyed service only once it has a key, the
/// active one always.
pub fn shown_providers<'a>(providers: &'a [Value], active: &str) -> Vec<&'a Value> {
    providers
        .iter()
        .filter(|p| {
            let flag = |k: &str| p.get(k).and_then(Value::as_bool).unwrap_or(false);
            !flag("token_required") || flag("token_set") || p.get("id").and_then(Value::as_str) == Some(active)
        })
        .collect()
}

/// The coordinate system choices of an elevation import.
pub const DEM_CRS: [(&str, &str); 12] = [
    ("auto", "From the file"),
    ("25832", "ETRS89 / UTM zone 32N (25832)"),
    ("25833", "ETRS89 / UTM zone 33N (25833)"),
    ("4647", "ETRS89 / UTM 32N, zone in easting (4647)"),
    ("31468", "DHDN / Gauss-Kr\u{fc}ger zone 4 (31468)"),
    ("31467", "DHDN / Gauss-Kr\u{fc}ger zone 3 (31467)"),
    ("2056", "Swiss LV95 (2056)"),
    ("2154", "France Lambert-93 (2154)"),
    ("27700", "British National Grid (27700)"),
    ("3035", "ETRS89 / LAEA Europe (3035)"),
    ("4326", "WGS 84 longitude and latitude (4326)"),
    ("other", "Other EPSG code\u{2026}"),
];

/// `^\d{4,6}$`: an EPSG code typed by hand.
pub fn epsg_ok(code: &str) -> bool {
    (4..=6).contains(&code.len()) && code.bytes().all(|b| b.is_ascii_digit())
}

/// The coordinate system an import sends: the choice, or the typed code.
pub fn crs_param(choice: &str, other: &str) -> String {
    if choice == "other" {
        other.trim().to_string()
    } else {
        choice.to_string()
    }
}

/// `describe(m)` of the elevation card: files, grid, height range.
pub fn dem_summary(m: &Value, units: &Units) -> String {
    let files = m.get("files").and_then(Value::as_array).map_or(0, Vec::len);
    let res = m.get("resolution_m").and_then(Value::as_f64).unwrap_or(0.0);
    let res_text = if res >= 10.0 {
        format!("{} m", crate::data::units::js_round(res))
    } else {
        format!(
            "{} m",
            crate::data::units::js_to_fixed(res, if res < 1.0 { 2 } else { 1 })
        )
    };
    let lo = m.get("min").and_then(Value::as_f64).unwrap_or(0.0);
    let hi = m.get("max").and_then(Value::as_f64).unwrap_or(0.0);
    let range = format!(
        "{} to {}",
        units.format_length(lo, 0, false, false),
        units.format_length(hi, 0, false, false)
    );
    [
        if files == 1 {
            "1 file".to_string()
        } else {
            format!("{files} files")
        },
        format!("{res_text} grid"),
        range,
    ]
    .join(" \u{b7} ")
}

/// `detail(m)`: coordinate systems, the first four file names, the size.
pub fn dem_detail(m: &Value) -> String {
    let files: Vec<&Value> = m
        .get("files")
        .and_then(Value::as_array)
        .map_or(Vec::new(), |a| a.iter().collect());
    let mut systems: Vec<String> = Vec::new();
    for f in &files {
        if let Some(s) = f.get("crs_name").and_then(Value::as_str).filter(|s| !s.is_empty()) {
            if !systems.iter().any(|x| x == s) {
                systems.push(s.into());
            }
        }
    }
    let names: Vec<&str> = files
        .iter()
        .filter_map(|f| f.get("file_name").and_then(Value::as_str))
        .filter(|s| !s.is_empty())
        .collect();
    let mut shown = names.iter().take(4).copied().collect::<Vec<_>>().join(", ");
    if names.len() > 4 {
        shown.push_str(&format!(", and {} more", names.len() - 4));
    }
    let mb = m.get("bytes").and_then(Value::as_f64).unwrap_or(0.0) / (1024.0 * 1024.0);
    let size = format!(
        "{} MB on disk",
        if mb < 10.0 {
            crate::data::units::js_to_fixed(mb, 1)
        } else {
            crate::data::units::js_round(mb).to_string()
        }
    );
    [systems.join(", "), shown, size]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" \u{b7} ")
}

/// The chip and sentence a model's kind carries.
pub fn dem_kind(kind: &str) -> Option<(&'static str, &'static str)> {
    match kind {
        "terrain" => Some((
            "TERRAIN",
            "Terrain model: bare ground, without buildings or trees. Turn on Buildings in the 3D view to see them as blocks.",
        )),
        "surface" => Some((
            "SURFACE",
            "Surface model: buildings and trees are part of the heights. Turn off Buildings in the 3D view, or they stand twice.",
        )),
        "mixed" => Some((
            "MIXED",
            "Some files are terrain models (bare ground), others surface models (with buildings and trees).",
        )),
        _ => None,
    }
}

/// `ssh` row text: `name · host:port (user)`.
pub fn ssh_line(c: &Value) -> String {
    let s = |k: &str| c.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let port = c.get("port").map_or(String::new(), |p| match p {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    });
    format!("{} \u{b7} {}:{} ({})", s("name"), s("host"), port, s("username"))
}

/// The plugin line on the right of its name (`pluginRow`): where it came
/// from, its version, whether it has a tab of its own and whether it is not
/// loaded at all.
pub fn plugin_origin(p: &Value) -> String {
    let where_ = if p.get("source").and_then(Value::as_str) == Some("bundled") {
        "built in"
    } else {
        "installed"
    };
    let version = p.get("version").and_then(Value::as_str).filter(|v| !v.is_empty());
    let flag = |k: &str| p.get(k).and_then(Value::as_bool).unwrap_or(false);
    let tab = if flag("in_tab") { " \u{b7} own tab" } else { "" };
    let failed = if p.get("loaded").and_then(Value::as_bool) == Some(false) {
        " \u{b7} not loaded"
    } else {
        ""
    };
    format!(
        "{where_}{}{tab}{failed}",
        version.map_or(String::new(), |v| format!(" {v}"))
    )
}

/// The rows of Settings, Plugins: what the backend lists, each marked with
/// what the registry knows of it (loaded, in a tab, has a gear), then the
/// compiled-in plugins the backend does not list. A plugin of the backend's
/// folder that is not compiled in cannot run here and reads "not loaded".
pub fn plugin_rows(listed: &[Value], registry: &crate::plugins::Registry) -> Vec<Value> {
    let mut rows: Vec<Value> = Vec::new();
    for p in listed {
        let Some(id) = p.get("id").and_then(Value::as_str) else {
            continue;
        };
        let mut row = p.clone();
        let known = registry.index_of(id);
        if let Some(o) = row.as_object_mut() {
            o.insert("loaded".into(), serde_json::json!(known.is_some()));
            if let Some(i) = known {
                o.insert("in_tab".into(), serde_json::json!(registry.in_tab(i)));
                o.insert("gear".into(), serde_json::json!(registry.has_options(i)));
            }
        }
        rows.push(row);
    }
    let mut own: Vec<usize> = (0..registry.len())
        .filter(|&i| {
            !rows
                .iter()
                .any(|r| r.get("id").and_then(Value::as_str) == Some(registry.info(i).id))
        })
        .collect();
    own.sort_by_key(|&i| (registry.info(i).order, registry.info(i).name.to_lowercase()));
    for i in own {
        let info = registry.info(i);
        rows.push(serde_json::json!({
            "id": info.id, "name": info.name, "description": info.description,
            "version": info.version, "source": "bundled", "loaded": true,
            "in_tab": registry.in_tab(i), "gear": registry.has_options(i),
        }));
    }
    rows
}

/// "N copies, K kB in DIR" (`paramDefaultsCacheCard`).
pub fn cache_line(data: &Value) -> String {
    let n = data.get("files").and_then(Value::as_f64).unwrap_or(0.0) as u64;
    let kb = crate::data::units::js_round(data.get("bytes").and_then(Value::as_f64).unwrap_or(0.0) / 1024.0);
    if n == 0 {
        return "No copies kept.".into();
    }
    let mut s = format!("{n} {}, {kb} kB", if n == 1 { "copy" } else { "copies" });
    if let Some(d) = data.get("dir").and_then(Value::as_str).filter(|d| !d.is_empty()) {
        s.push_str(&format!(" in {d}"));
    }
    s
}

/// The verdict a finished update check shows (`Check now`).
pub fn update_verdict(data: &Value) -> (String, crate::ui::form::Tone) {
    use crate::ui::form::Tone;
    let latest = data.get("latest").and_then(Value::as_str).unwrap_or("");
    if data.get("enabled").and_then(Value::as_bool) == Some(false) {
        ("Update checks are switched off.".into(), Tone::Warn)
    } else if let Some(e) = data.get("error").and_then(Value::as_str).filter(|e| !e.is_empty()) {
        (e.into(), Tone::Warn)
    } else if data.get("update_available").and_then(Value::as_bool) == Some(true) {
        (format!("Version {latest} is available."), Tone::Ok)
    } else {
        ("Corvus GCS is up to date.".into(), Tone::Ok)
    }
}

/// "On. No AI client connected yet." and its siblings (`clientsLine`).
pub fn clients_line(status: &Value) -> String {
    if status.get("enabled").and_then(Value::as_bool) != Some(true) {
        return "Off. Nothing can connect.".into();
    }
    let live: Vec<String> = status
        .get("clients")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter(|c| c.get("seen_s").and_then(Value::as_f64).unwrap_or(f64::MAX) <= 120.0)
                .map(|c| {
                    let name = c.get("name").and_then(Value::as_str).unwrap_or("");
                    let name = match c.get("version").and_then(Value::as_str).filter(|v| !v.is_empty()) {
                        Some(v) => format!("{name} {v}"),
                        None => name.to_string(),
                    };
                    let seen = crate::data::units::js_round(c.get("seen_s").and_then(Value::as_f64).unwrap_or(0.0));
                    format!("{name} ({seen} s ago)")
                })
                .collect()
        })
        .unwrap_or_default();
    if live.is_empty() {
        "On. No AI client connected yet.".into()
    } else {
        format!("Connected: {}", live.join(", "))
    }
}

/// "3 of 6 permissions on, 1 of them ask you first." (`permissionSummary`).
pub fn permission_summary(settings: &Value, catalog: &[Value]) -> String {
    let perms = settings.get("permissions");
    let confirm = settings.get("confirm");
    let on = |m: Option<&Value>, id: &str| m.and_then(|m| m.get(id)).and_then(Value::as_bool).unwrap_or(false);
    let id_of = |p: &Value| p.get("id").and_then(Value::as_str).unwrap_or("").to_string();
    let enabled: Vec<&Value> = catalog.iter().filter(|p| on(perms, &id_of(p))).collect();
    let asking = enabled
        .iter()
        .filter(|p| p.get("writes").and_then(Value::as_bool).unwrap_or(false) && on(confirm, &id_of(p)))
        .count();
    let base = format!("{} of {} permissions on", enabled.len(), catalog.len());
    if asking > 0 {
        format!("{base}, {asking} of them ask you first.")
    } else {
        format!("{base}.")
    }
}

/// The block to paste into OpenCode (`opencodeConfig`), two-space JSON.
pub fn opencode_config(url: &str, token: &str) -> String {
    serde_json::to_string_pretty(&json!({
        "$schema": "https://opencode.ai/config.json",
        "mcp": { "corvus": {
            "type": "remote",
            "url": url,
            "enabled": true,
            "oauth": false,
            "timeout": 60000,
            "headers": { "Authorization": format!("Bearer {token}") },
        }},
    }))
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_write_merges_one_level_deep_and_hands_back_its_undo() {
        let mut cfg = json!({"ui": {"scale": 1.0, "units": {"length": "m"}}, "theme": {"name": "pink"}});
        let undo = merge(&mut cfg, &json!({"ui": {"scale": 1.25, "mission_page": true}}));
        assert_eq!(cfg["ui"]["scale"], 1.25);
        assert_eq!(cfg["ui"]["units"]["length"], "m");
        assert_eq!(undo, json!({"ui": {"scale": 1.0, "mission_page": null}}));
        merge(&mut cfg, &undo);
        assert_eq!(cfg["ui"]["scale"], 1.0);
        assert!(cfg["ui"]["mission_page"].is_null());
    }

    #[test]
    fn the_scale_is_clamped_not_snapped() {
        assert_eq!(scale_from_config(&json!({"ui": {"scale": 1.05}})), 1.05);
        assert_eq!(scale_from_config(&json!({"ui": {"scale": 9}})), 1.5);
        assert_eq!(scale_from_config(&json!({"ui": {"scale": -1}})), 1.0);
        assert_eq!(scale_from_config(&json!({})), 1.0);
        assert_eq!(nearest_step(&SCALE_STEPS, 1.05), 2);
        assert_eq!(nearest_step(&SCALE_STEPS, 1.2), 4);
        assert_eq!(nearest_step(&GAIN_STEPS, 0.5), 1);
    }

    #[test]
    fn the_unit_presets_match_the_four_selects() {
        let cfg = json!({"ui": {"units": {"length": "ft", "distance": "nmi", "speed": "kn", "temperature": "c"}}});
        let u = units_of(&cfg);
        assert_eq!(system_of(&u), Some("aviation"));
        let mixed = units_of(&json!({"ui": {"units": {"length": "ft", "speed": "warp"}}}));
        assert_eq!(mixed, ["ft", "km", "ms", "c"].map(String::from));
        assert_eq!(system_of(&mixed), None);
        assert_eq!(system_of(&units_of(&json!({}))), Some("metric"));
        assert_eq!(units_value(&u)["speed"], "kn");
    }

    #[test]
    fn the_map_service_cards_read_as_the_web_writes_them() {
        let providers = vec![
            json!({"id": "esri", "label": "Esri", "sources": ["esri-sat", "esri-street"]}),
            json!({"id": "mapbox", "label": "Mapbox", "sources": ["mb"], "token_required": true}),
            json!({"id": "google", "label": "Google", "sources": ["g"], "token_optional": true, "token_set": true}),
        ];
        let sources = vec![json!({"id": "esri-sat", "cached_count": 1200, "maxzoom": 19, "provider": "esri"})];
        assert_eq!(describe_provider(&providers[0], &sources), "2 layers \u{b7} 1 cached");
        assert_eq!(describe_provider(&providers[1], &sources), "Needs an API key");
        assert_eq!(
            describe_provider(&providers[2], &sources),
            "1 layer \u{b7} licensed API"
        );
        assert_eq!(
            describe_cache(&sources[0]),
            "1,200 tiles cached offline \u{b7} up to z19"
        );
        assert_eq!(
            describe_cache(&json!({"maxzoom": 17})),
            "Nothing cached yet \u{b7} up to z17"
        );
        assert_eq!(
            active_provider(&json!({"map": {"base_layer": "esri-sat"}}), &providers, &sources, ""),
            "esri"
        );
        assert_eq!(active_provider(&json!({}), &providers, &sources, "google"), "google");
        assert_eq!(active_provider(&json!({}), &providers, &sources, ""), "esri");
        let shown: Vec<&str> = shown_providers(&providers, "esri")
            .iter()
            .map(|p| p["id"].as_str().unwrap())
            .collect();
        assert_eq!(shown, ["esri", "google"]);
        assert_eq!(shown_providers(&providers, "mapbox").len(), 3);
    }

    #[test]
    fn the_keys_note_names_who_waits() {
        let keyed = vec![
            json!({"label": "Mapbox", "token_required": true}),
            json!({"label": "Google", "token_optional": true}),
            json!({"label": "Bing", "token_optional": true}),
        ];
        assert_eq!(
            keys_note(&keyed),
            "Mapbox appears as a map service once you add an API key. Google and Bing use their licensed API once you add a key."
        );
        assert_eq!(
            keys_note(&[json!({"label": "Google", "token_set": true})]),
            "Every map service has a key."
        );
        assert_eq!(list_names(&["A".into(), "B".into(), "C".into()]), "A, B and C");
    }

    #[test]
    fn counts_are_grouped_like_to_locale_string() {
        assert_eq!(grouped(0), "0");
        assert_eq!(grouped(999), "999");
        assert_eq!(grouped(1000), "1,000");
        assert_eq!(grouped(1234567), "1,234,567");
    }

    #[test]
    fn an_elevation_model_is_described_as_the_card_describes_it() {
        let m = json!({
            "files": [{"crs_name": "ETRS89 / UTM zone 32N", "file_name": "a.tif"},
                      {"crs_name": "ETRS89 / UTM zone 32N", "file_name": "b.tif"}],
            "resolution_m": 1.0, "min": 480.4, "max": 611.6, "bytes": 5.0 * 1048576.0, "kind": "terrain",
        });
        let metric = Units::default();
        assert_eq!(
            dem_summary(&m, &metric),
            "2 files \u{b7} 1.0 m grid \u{b7} 480 m to 612 m"
        );
        assert_eq!(
            dem_detail(&m),
            "ETRS89 / UTM zone 32N \u{b7} a.tif, b.tif \u{b7} 5.0 MB on disk"
        );
        let coarse = json!({"files": [{}], "resolution_m": 30.4, "min": 0, "max": 10, "bytes": 52428800});
        assert!(dem_summary(&coarse, &metric).starts_with("1 file \u{b7} 30 m grid"));
        assert!(dem_detail(&coarse).ends_with("50 MB on disk"));
        let fine = json!({"files": [], "resolution_m": 0.5, "min": 0, "max": 1});
        assert!(dem_summary(&fine, &metric).contains("0.50 m grid"));
        let many = json!({"files": (1..=6).map(|i| json!({"file_name": format!("f{i}.tif")})).collect::<Vec<_>>()});
        assert!(dem_detail(&many).contains("f1.tif, f2.tif, f3.tif, f4.tif, and 2 more"));
        assert_eq!(dem_kind("surface").unwrap().0, "SURFACE");
        assert!(dem_kind("other").is_none());
    }

    #[test]
    fn an_epsg_code_is_four_to_six_digits() {
        assert!(epsg_ok("25832"));
        assert!(epsg_ok("4326"));
        assert!(!epsg_ok("432"));
        assert!(!epsg_ok("1234567"));
        assert!(!epsg_ok("25a32"));
        assert_eq!(crs_param("other", " 25832 "), "25832");
        assert_eq!(crs_param("auto", "x"), "auto");
    }

    #[test]
    fn the_status_lines_are_the_webs() {
        assert_eq!(
            ssh_line(&json!({"name": "drone", "host": "10.0.0.2", "port": 22, "username": "pi"})),
            "drone \u{b7} 10.0.0.2:22 (pi)"
        );
        assert_eq!(
            plugin_origin(&json!({"source": "bundled", "version": "1.2"})),
            "built in 1.2"
        );
        assert_eq!(plugin_origin(&json!({"source": "user"})), "installed");
        assert_eq!(cache_line(&json!({"files": 0})), "No copies kept.");
        assert_eq!(
            cache_line(&json!({"files": 1, "bytes": 2048, "dir": "/x"})),
            "1 copy, 2 kB in /x"
        );
        assert_eq!(cache_line(&json!({"files": 3, "bytes": 1536})), "3 copies, 2 kB");
        let (t, _) = update_verdict(&json!({"update_available": true, "latest": "2026.10.02"}));
        assert_eq!(t, "Version 2026.10.02 is available.");
        assert_eq!(
            update_verdict(&json!({"enabled": false})).0,
            "Update checks are switched off."
        );
        assert_eq!(update_verdict(&json!({"error": "no route"})).0, "no route");
        assert_eq!(update_verdict(&json!({})).0, "Corvus GCS is up to date.");
    }

    #[test]
    fn the_mcp_card_summaries() {
        assert_eq!(clients_line(&json!({"enabled": false})), "Off. Nothing can connect.");
        assert_eq!(
            clients_line(&json!({"enabled": true})),
            "On. No AI client connected yet."
        );
        assert_eq!(
            clients_line(&json!({"enabled": true, "clients": [
                {"name": "opencode", "version": "1.0", "seen_s": 12.4},
                {"name": "old", "seen_s": 900}]})),
            "Connected: opencode 1.0 (12 s ago)"
        );
        let catalog = vec![
            json!({"id": "read", "writes": false}),
            json!({"id": "fly", "writes": true}),
            json!({"id": "plan", "writes": true}),
        ];
        let settings = json!({"permissions": {"read": true, "fly": true}, "confirm": {"fly": true, "plan": true}});
        assert_eq!(
            permission_summary(&settings, &catalog),
            "2 of 3 permissions on, 1 of them ask you first."
        );
        assert_eq!(permission_summary(&json!({}), &catalog), "0 of 3 permissions on.");
        let cfg = opencode_config("http://127.0.0.1:8080/mcp", "<token>");
        assert!(cfg.contains("\"Authorization\": \"Bearer <token>\""));
        assert!(cfg.starts_with("{\n  \"$schema\""));
    }

    #[test]
    fn small_pickers_fall_back_as_the_web_does() {
        assert_eq!(review_of(&json!({"review": {"sensitivity": "strict"}})), "strict");
        assert_eq!(review_of(&json!({"review": {"sensitivity": "wild"}})), "normal");
        assert_eq!(
            altitude_ref(&json!({"ui": {"topbar_altitude": "relative"}})),
            "relative"
        );
        assert_eq!(altitude_ref(&json!({})), "amsl");
        assert_eq!(map_rate(&json!({"ui": {"map_animation_rate": 30}})), 30);
        assert_eq!(map_rate(&json!({"ui": {"map_animation_rate": 45}})), 60);
        assert!(flag(&json!({}), "ui", "flight_hud", true));
        assert!(!flag(&json!({"ui": {"flight_hud": false}}), "ui", "flight_hud", true));
        assert_eq!(patch("ui", "scale", json!(1.25)), json!({"ui": {"scale": 1.25}}));
        assert_eq!(TABS.len(), 9);
        assert_eq!(THEMES.len(), crate::ui::style::THEME_IDS.len());
        for (t, id) in THEMES.iter().zip(crate::ui::style::THEME_IDS) {
            assert_eq!(t.0, id);
        }
    }
}
