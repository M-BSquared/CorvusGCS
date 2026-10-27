//! The status bar along the top: vehicle, mode, readiness, GPS, battery and
//! the three flight readouts.
//!
//! What each block says is the web interface's top bar (src/js/topbar.js)
//! word for word, because an operator moving between the two must read the
//! same thing for the same aircraft. The readiness block answers "can this
//! fly, and is it flying", not "is the switch on": FLYING over ARMED once
//! off the ground, READY only on the autopilot's own clearance, STANDBY
//! when the firmware gives none.

use eframe::egui::{self, Color32, Pos2, Rect, Vec2};
use serde_json::Value;

use crate::style::{self, Weight};
use crate::units::{js_to_fixed, Units};

/// What a value is saying: its class in the web bar (`.tb-value.<cls>`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Plain,
    Accent,
    Healthy,
    Warning,
    Critical,
    Armed,
    Off,
}

impl Tone {
    /// The value's colour. `off` has no rule of its own in main.css, so it
    /// keeps the bar's text colour, as `accent` does on the light theme.
    pub fn color(self) -> Color32 {
        match self {
            Tone::Plain | Tone::Accent | Tone::Off => style::text_1(),
            Tone::Healthy => style::healthy(),
            Tone::Warning | Tone::Armed => style::warning(),
            Tone::Critical => style::critical(),
        }
    }
}

/// One block of the bar.
#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    pub label: &'static str,
    pub value: String,
    pub sub: String,
    pub tone: Tone,
    pub title: String,
}

fn s<'a>(state: &'a Value, key: &str) -> &'a str {
    state.get(key).and_then(Value::as_str).unwrap_or("")
}

fn num(state: &Value, key: &str) -> f64 {
    match state.get(key) {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
        Some(Value::String(t)) => t.trim().parse().unwrap_or(f64::NAN),
        Some(Value::Bool(b)) => f64::from(u8::from(*b)),
        _ => 0.0,
    }
}

fn truthy(state: &Value, key: &str) -> bool {
    match state.get(key) {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|v| v != 0.0 && !v.is_nan()),
        Some(Value::String(t)) => !t.is_empty(),
        Some(Value::Array(_) | Value::Object(_)) => true,
        _ => false,
    }
}

const VEHICLE_TYPES: &[(&str, &str)] = &[
    ("GENERIC", "Generic"),
    ("FIXED_WING", "Fixed Wing"),
    ("QUADROTOR", "Quadcopter"),
    ("COAXIAL", "Coaxial Helicopter"),
    ("HELICOPTER", "Helicopter"),
    ("AIRSHIP", "Airship"),
    ("FREE_BALLOON", "Balloon"),
    ("ROCKET", "Rocket"),
    ("GROUNDED_ROVER", "Rover"),
    ("SURFACE_BOAT", "Boat"),
    ("SUBMARINE", "Submarine"),
    ("HEXAROTOR", "Hexacopter"),
    ("OCTOROTOR", "Octocopter"),
    ("TRIROTOR", "Tricopter"),
    ("VTOL_DUOROTOR", "VTOL Duorotor"),
    ("VTOL_QUADROTOR", "VTOL Quadrotor"),
    ("VTOL_TILTROTOR", "VTOL Tiltrotor"),
];

/// The airframe as an operator names it; a frame this build predates is
/// titled from its enum name ("TYPE_23" reads "Type 23").
pub fn vehicle_type_label(raw: &str) -> String {
    if raw.is_empty() {
        return "Unknown".into();
    }
    if let Some((_, label)) = VEHICLE_TYPES.iter().find(|(k, _)| *k == raw) {
        return (*label).into();
    }
    raw.to_lowercase()
        .split('_')
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut c = w.chars();
            c.next()
                .map(|f| f.to_uppercase().chain(c).collect::<String>())
                .unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn vehicle_label(state: &Value) -> String {
    if !truthy(state, "connected") {
        return "DISCONNECTED".into();
    }
    let kind = vehicle_type_label(s(state, "vehicle_type"));
    match s(state, "autopilot") {
        "" => kind,
        ap => format!("{ap} {kind}"),
    }
}

/// The firmware release, major.minor only (the full string and build go in
/// the tooltip), or a dash when the autopilot never said.
pub fn firmware(state: &Value) -> (String, String) {
    if !truthy(state, "connected") {
        return (String::new(), String::new());
    }
    let full = s(state, "px4_version");
    if !full.is_empty() {
        let detail = match s(state, "px4_version_detail") {
            "" => full.to_string(),
            d => format!("{full}, build {d}"),
        };
        return (short_version(full), detail);
    }
    match s(state, "autopilot") {
        "" => (String::new(), String::new()),
        ap => (
            "\u{2014}".into(),
            format!("{ap} firmware version undetected (AUTOPILOT_VERSION not provided by this firmware)"),
        ),
    }
}

/// `v1.18.2` to `v1.18`, as `/^(v?\d+\.\d+)\.\d+/` does.
fn short_version(full: &str) -> String {
    let b = full.as_bytes();
    let mut i = usize::from(b.first() == Some(&b'v'));
    let digits = |i: &mut usize| {
        let start = *i;
        while *i < b.len() && b[*i].is_ascii_digit() {
            *i += 1;
        }
        *i > start
    };
    if !digits(&mut i) || b.get(i) != Some(&b'.') {
        return full.into();
    }
    i += 1;
    if !digits(&mut i) {
        return full.into();
    }
    let keep = i;
    if b.get(i) != Some(&b'.') {
        return full.into();
    }
    i += 1;
    if !digits(&mut i) {
        return full.into();
    }
    format!("{}{}", &full[..keep], &full[i..])
}

const LANDED_ON_GROUND: i64 = 1;
const AIRBORNE_LANDED: [i64; 3] = [2, 3, 4];
const AIRBORNE_FALLBACK_M: f64 = 1.5;

/// Off the ground: the vehicle's own landed state where it has one, its
/// height above home otherwise.
pub fn is_airborne(state: &Value) -> bool {
    let landed = state.get("landed_state").and_then(Value::as_i64);
    if landed.is_some_and(|l| AIRBORNE_LANDED.contains(&l)) {
        return true;
    }
    if landed == Some(LANDED_ON_GROUND) {
        return false;
    }
    let agl = num(state, "altitude_agl");
    agl > AIRBORNE_FALLBACK_M
}

/// The readiness block.
pub fn readiness(state: &Value) -> Block {
    let block = |value: &str, sub: String, tone: Tone, title: String| Block {
        label: "Status",
        value: value.into(),
        sub,
        tone,
        title,
    };
    if !truthy(state, "connected") {
        return block("—", String::new(), Tone::Off, "No link to a vehicle".into());
    }
    let armed = truthy(state, "armed");
    let armed_sub = if armed { "armed".to_string() } else { String::new() };
    if state.get("kill_switch") == Some(&Value::Bool(true)) {
        let title = if armed {
            "Kill switch engaged. Motors are stopped, but the vehicle is still armed: releasing the switch lets them spin again"
        } else {
            "Kill switch engaged. Release it before arming"
        };
        return block("KILL SWITCH", armed_sub, Tone::Critical, title.into());
    }
    if state.get("flight_termination") == Some(&Value::Bool(true)) {
        return block(
            "TERMINATED",
            armed_sub,
            Tone::Critical,
            "The autopilot reports flight termination or lockdown. Motor outputs are cut".into(),
        );
    }
    if armed {
        if is_airborne(state) {
            return block(
                "FLYING",
                String::new(),
                Tone::Healthy,
                "Airborne, motors are live".into(),
            );
        }
        return block(
            "ARMED",
            String::new(),
            Tone::Armed,
            "Armed on the ground, propellers are live".into(),
        );
    }
    match state.get("prearm_ok") {
        Some(Value::Bool(true)) => block(
            "READY",
            String::new(),
            Tone::Healthy,
            "Preflight checks pass. The vehicle would accept an arm command".into(),
        ),
        Some(Value::Bool(false)) => {
            let reasons: Vec<&str> = state
                .get("prearm_reasons")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_str).filter(|r| !r.is_empty()).collect())
                .unwrap_or_default();
            let n = reasons.len();
            let sub = if n > 0 {
                format!("{n} check{}", if n == 1 { "" } else { "s" })
            } else {
                String::new()
            };
            let title = if n > 0 {
                let list: Vec<String> = reasons.iter().map(|r| format!("• {r}")).collect();
                format!("The autopilot is refusing to arm:\n{}", list.join("\n"))
            } else {
                "The autopilot is refusing to arm. It has not said why yet; the reason arrives in the notifications"
                    .into()
            };
            block("NOT READY", sub, Tone::Warning, title)
        }
        _ => block(
            "STANDBY",
            String::new(),
            Tone::Off,
            "Disarmed. This firmware does not report its preflight-check state, so readiness is unknown.".into(),
        ),
    }
}

/// The mode word the dialect gives, or what stands in for it.
pub fn mode_label(state: &Value) -> String {
    if !truthy(state, "connected") {
        return "—".into();
    }
    if truthy(state, "armed") && !truthy(state, "mode") {
        return if is_airborne(state) { "FLYING" } else { "ARMED" }.into();
    }
    for key in ["mode_label", "mode"] {
        if truthy(state, key) {
            return s(state, key).into();
        }
    }
    "STANDBY".into()
}

fn has_fix(state: &Value) -> bool {
    let fix = s(state, "gps_fix");
    !fix.is_empty() && fix != "NO_GPS" && fix != "NO_FIX"
}

/// The reception verdict and its tone.
pub fn reception(state: &Value) -> (&'static str, Tone) {
    if !has_fix(state) {
        return ("No fix", Tone::Critical);
    }
    let sats = num(state, "gps_satellites");
    let sats = if sats.is_nan() { 0.0 } else { sats };
    let hdop = num(state, "gps_hdop");
    let hdop = if hdop == 0.0 || hdop.is_nan() { 99.0 } else { hdop };
    if sats >= 10.0 && hdop <= 1.5 {
        ("Good", Tone::Healthy)
    } else if sats >= 6.0 && hdop <= 2.5 {
        ("Fair", Tone::Warning)
    } else {
        ("Poor", Tone::Critical)
    }
}

fn fix_short(fix: &str) -> String {
    match fix {
        "NO_GPS" => "none",
        "NO_FIX" => "no fix",
        "2D_FIX" => "2D",
        "3D_FIX" => "",
        "DGPS" => "DGPS",
        "RTK_FLOAT" => "RTK float",
        "RTK_FIXED" => "RTK fix",
        "STATIC" => "static",
        "PPP" => "PPP",
        other => other,
    }
    .into()
}

/// Every block, left to right.
pub fn blocks(state: &Value, units: &Units) -> Vec<Block> {
    let connected = truthy(state, "connected");
    let dash = || "—".to_string();
    let fw = firmware(state);
    let gps_fix = match s(state, "gps_fix") {
        "" => "NO_GPS",
        f => f,
    };
    let gps_tone = if !connected || !truthy(state, "gps_fix") || gps_fix == "NO_GPS" {
        Tone::Off
    } else {
        reception(state).1
    };
    let pct = num(state, "battery_percent");
    let pct = if pct.is_nan() { 0.0 } else { pct };
    let batt_tone = if !connected {
        Tone::Off
    } else if pct > 25.0 {
        Tone::Healthy
    } else if pct > 12.0 {
        Tone::Warning
    } else {
        Tone::Critical
    };
    vec![
        Block {
            label: "Vehicle",
            value: vehicle_label(state),
            sub: fw.0,
            tone: if connected { Tone::Plain } else { Tone::Critical },
            title: fw.1,
        },
        Block {
            label: "Mode",
            value: mode_label(state),
            sub: String::new(),
            tone: Tone::Accent,
            title: String::new(),
        },
        readiness(state),
        Block {
            label: "GPS",
            value: if connected { "GPS".into() } else { dash() },
            sub: if connected { fix_short(gps_fix) } else { String::new() },
            tone: gps_tone,
            title: String::new(),
        },
        Block {
            label: "Battery",
            value: if connected {
                format!("{} V", js_to_fixed(num(state, "battery_voltage"), 1))
            } else {
                dash()
            },
            sub: if connected {
                format!("{}%", crate::units::js_number(pct))
            } else {
                String::new()
            },
            tone: batt_tone,
            title: String::new(),
        },
        Block {
            label: "Altitude",
            value: if connected {
                units.format_length(num(state, "altitude_amsl"), 0, false, true)
            } else {
                dash()
            },
            sub: format!("{} AMSL", units.length.symbol),
            tone: Tone::Plain,
            title: "Altitude above mean sea level".into(),
        },
        Block {
            label: "Groundspeed",
            value: if connected {
                units.format_speed(num(state, "groundspeed"), 1, false, true)
            } else {
                dash()
            },
            sub: units.speed.symbol.into(),
            tone: Tone::Plain,
            title: String::new(),
        },
        Block {
            label: "Vertical speed",
            value: if connected {
                units.format_speed(num(state, "vspeed"), 1, true, true)
            } else {
                dash()
            },
            sub: units.speed.symbol.into(),
            tone: Tone::Plain,
            title: String::new(),
        },
    ]
}

/// The bar's height (`--top-h`).
pub const HEIGHT: f32 = 60.0;

/// The STATUS tile's tint (`.tb-block[data-tone]`): a pill for the two
/// states worth recognising by shape on the ground, and for a cut motor.
fn tile(b: &Block) -> Option<Color32> {
    if b.label != "Status" {
        return None;
    }
    match b.value.as_str() {
        "READY" => Some(style::healthy_dark()),
        "ARMED" => Some(style::warning_dark()),
        "KILL SWITCH" | "TERMINATED" => Some(style::critical_dark()),
        _ => None,
    }
}

/// Draws the bar into `rect` (the full width, `HEIGHT` tall).
/// Returns true when the NOTIFICATIONS block was clicked.
pub fn show(
    ui: &mut egui::Ui,
    rect: Rect,
    logo: &egui::ImageSource<'static>,
    blocks: &[Block],
    notifications: &crate::notify::Summary,
    board_open: bool,
) -> bool {
    let painter = ui.painter_at(rect);
    // background: linear-gradient(180deg, var(--surface-1), var(--bg))
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(rect.left_top(), style::surface_1());
    mesh.colored_vertex(rect.right_top(), style::surface_1());
    mesh.colored_vertex(rect.right_bottom(), style::bg());
    mesh.colored_vertex(rect.left_bottom(), style::bg());
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    painter.add(mesh);
    painter.hline(
        rect.x_range(),
        rect.bottom() - 0.5,
        egui::Stroke::new(1.0, style::border()),
    );

    // .top-bar { padding: 0 12px 0 8px; gap: 2px }; .tb-logo { padding: 0 14px 0 4px; gap: 10px }
    let mut x = rect.left() + 8.0 + 4.0;
    let mark = Rect::from_min_size(Pos2::new(x, rect.center().y - 17.0), Vec2::splat(34.0));
    egui::Image::new(logo.clone()).paint_at(ui, mark);
    x = mark.right() + 10.0;
    let name = style::galley(ui, "CORVUS", 15.0, Weight::W700, style::text_1(), 1.4);
    let sub = style::galley(ui, "GROUND CONTROL", 9.0, Weight::W600, style::text_3(), 1.6);
    // line-height: 1, so each line is its font size tall; 3px between.
    let text_h = 15.0 + 3.0 + 9.0;
    let top = rect.center().y - text_h / 2.0;
    painter.galley(
        Pos2::new(x, top + (15.0 - name.size().y) / 2.0),
        name.clone(),
        style::text_1(),
    );
    painter.galley(
        Pos2::new(x, top + 18.0 + (9.0 - sub.size().y) / 2.0),
        sub.clone(),
        style::text_3(),
    );
    x += name.size().x.max(sub.size().x) + 14.0;
    painter.vline(x - 0.5, rect.y_range(), egui::Stroke::new(1.0, style::border_soft()));
    // .tb-logo { margin-right: 6px } and the bar's 2px gap
    x += 6.0 + 2.0;

    for (i, b) in blocks.iter().enumerate() {
        if i > 0 {
            x += 2.0 + 1.0;
        }
        let label = style::galley(ui, &b.label.to_uppercase(), 9.5, Weight::W600, style::text_3(), 0.9);
        let value = style::galley(ui, &b.value, 15.0, Weight::W600, b.tone.color(), 0.0);
        let sub = (!b.sub.is_empty()).then(|| style::galley(ui, &b.sub, 11.0, Weight::W500, style::text_2(), 0.0));
        let value_w = value.size().x + sub.as_ref().map_or(0.0, |s| 5.0 + s.size().x);
        let w = label.size().x.max(value_w) + 24.0;
        let block = Rect::from_min_max(Pos2::new(x, rect.top()), Pos2::new(x + w, rect.bottom() - 1.0));
        let id = ui.scope_id().with(("tb", i));
        let response = ui.interact(block, id, egui::Sense::hover());
        if response.hovered() {
            painter.rect_filled(block, 0, style::surface_hover());
        } else if let Some(fill) = tile(b) {
            painter.rect_filled(block, 0, fill);
        }
        // label 9.5px + 2px margin, value line-height 1.1
        let label_h = label.size().y;
        let value_h = 15.0 * 1.1;
        let top = block.center().y - (label_h + 2.0 + value_h) / 2.0;
        painter.galley(Pos2::new(x + 12.0, top), label, style::text_3());
        let base = top + label_h + 2.0;
        let vy = base + (value_h - value.size().y) / 2.0;
        painter.galley(Pos2::new(x + 12.0, vy), value.clone(), b.tone.color());
        if let Some(sub) = sub {
            // align-items: baseline
            let ascent_gap = value.rows.first().map_or(0.0, |r| r.row.height()) * 0.8
                - sub.rows.first().map_or(0.0, |r| r.row.height()) * 0.8;
            painter.galley(
                Pos2::new(x + 12.0 + value.size().x + 5.0, vy + ascent_gap),
                sub,
                style::text_2(),
            );
        }
        if !b.title.is_empty() {
            response.on_hover_text(&b.title);
        }
        x += w;
    }
    // The NOTIFICATIONS block closes the row, as the last of blocks().
    let (_, clicked) = crate::notify_ui::block(ui, rect, x + 3.0, notifications, board_open);
    clicked
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn vehicle_and_firmware() {
        let st = json!({"connected": true, "autopilot": "PX4", "vehicle_type": "QUADROTOR", "px4_version": "v1.18.2"});
        assert_eq!(vehicle_label(&st), "PX4 Quadcopter");
        assert_eq!(firmware(&st).0, "v1.18");
        assert_eq!(firmware(&st).1, "v1.18.2");
        assert_eq!(vehicle_type_label("TYPE_23"), "Type 23");
        assert_eq!(vehicle_type_label(""), "Unknown");
        assert_eq!(short_version("1.16.0rc1"), "1.16rc1");
        assert_eq!(short_version("4.5"), "4.5");
        let ap = json!({"connected": true, "autopilot": "ArduPilot"});
        assert_eq!(firmware(&ap).0, "\u{2014}");
        assert_eq!(vehicle_label(&json!({"connected": false})), "DISCONNECTED");
    }

    #[test]
    fn readiness_in_order_of_precedence() {
        let r = |v: Value| readiness(&v).value;
        assert_eq!(r(json!({"connected": false})), "—");
        assert_eq!(
            r(json!({"connected": true, "armed": true, "kill_switch": true})),
            "KILL SWITCH"
        );
        assert_eq!(r(json!({"connected": true, "flight_termination": true})), "TERMINATED");
        assert_eq!(
            r(json!({"connected": true, "armed": true, "landed_state": 2})),
            "FLYING"
        );
        assert_eq!(
            r(json!({"connected": true, "armed": true, "landed_state": 1, "altitude_agl": 30})),
            "ARMED"
        );
        assert_eq!(
            r(json!({"connected": true, "armed": true, "altitude_agl": 3})),
            "FLYING"
        );
        assert_eq!(r(json!({"connected": true, "prearm_ok": true})), "READY");
        assert_eq!(r(json!({"connected": true, "prearm_ok": null})), "STANDBY");
        let nr = readiness(&json!({"connected": true, "prearm_ok": false, "prearm_reasons": ["Compass", "", 3]}));
        assert_eq!((nr.value.as_str(), nr.sub.as_str()), ("NOT READY", "1 check"));
        assert_eq!(nr.title, "The autopilot is refusing to arm:\n• Compass");
    }

    #[test]
    fn mode_words() {
        assert_eq!(
            mode_label(&json!({"connected": true, "mode": "POSCTL", "mode_label": "POSITION"})),
            "POSITION"
        );
        assert_eq!(
            mode_label(&json!({"connected": true, "armed": true, "mode": ""})),
            "ARMED"
        );
        assert_eq!(mode_label(&json!({"connected": true})), "STANDBY");
        assert_eq!(mode_label(&json!({"connected": false, "mode": "HOLD"})), "—");
    }

    #[test]
    fn the_blocks_of_a_flying_vehicle() {
        let st = json!({
            "connected": true, "armed": true, "landed_state": 2, "autopilot": "PX4",
            "vehicle_type": "QUADROTOR", "px4_version": "v1.18.0", "mode": "MISSION",
            "gps_fix": "RTK_FIXED", "gps_satellites": 18, "gps_hdop": 0.7,
            "battery_voltage": 24.94, "battery_percent": 99, "altitude_amsl": 610.4,
            "groundspeed": 6.04, "vspeed": 0.0
        });
        let b = blocks(&st, &Units::default());
        let shown: Vec<(&str, &str)> = b.iter().map(|b| (b.value.as_str(), b.sub.as_str())).collect();
        assert_eq!(
            shown,
            vec![
                ("PX4 Quadcopter", "v1.18"),
                ("MISSION", ""),
                ("FLYING", ""),
                ("GPS", "RTK fix"),
                ("24.9 V", "99%"),
                ("610", "m AMSL"),
                ("6.0", "m/s"),
                ("+0.0", "m/s"),
            ]
        );
        assert_eq!(b[3].tone, Tone::Healthy);
        assert_eq!(b[4].tone, Tone::Healthy);
    }

    #[test]
    fn the_blocks_with_no_link() {
        let b = blocks(&crate::telemetry::initial_state(), &Units::default());
        assert!(b.iter().skip(3).take(2).all(|b| b.value == "—" && b.tone == Tone::Off));
        assert_eq!(b[5].value, "—");
        assert_eq!(b[5].sub, "m AMSL");
    }
}
