//! The flight bar at the top left of the map, the takeoff card under it and
//! the mode picker, as the web map has them (index.html `#flightActions`,
//! main.css `.flight-actions`, python/src/js/app.js `initFlightActions`).
//!
//! The behaviour is app.js's: one TAKEOFF button that turns into LAND in the
//! air (and for an armed vehicle whose phase cannot be told), the takeoff
//! card with its logarithmic slider from 1 to 120 m, ARM that reads DISARM
//! once armed, and a mode list from `/api/mavlink/modes` that shows a mode
//! the vehicle reports but cannot be commanded into as a disabled row of its
//! own. Every command is a POST to the same endpoint the web button posts
//! to, on a thread of its own, and the button is busy until the answer.

use crate::ui::tip::HoverTr;
use std::sync::{Arc, Mutex};

use eframe::egui::{self, Pos2, Rect, Stroke, Vec2};
use serde_json::{json, Value};

use crate::data::units::{js_round, Units};
use crate::map::tools::{Action, Tools};
use crate::net::http::{self, Server};
use crate::net::lock::Relock;
use crate::ui::fabar;
use crate::ui::field::TextField;
use crate::ui::icons;
use crate::ui::listrow::{self, Mark};
use crate::ui::menu;
use crate::ui::style::{self, Weight};

/// `--map-rail-inset`.
pub const INSET: f32 = 14.0;
const BAR_PAD: f32 = 7.0;
const BAR_GAP: f32 = 7.0;
/// `--fa-w`, every button one width at full size.
const BTN_W: f32 = 72.0;
/// `BAR_EDGE`, `BAR_MIN_SCALE`, `BAR_TIGHT_SCALE` of fitBar (ui.js).
const BAR_EDGE: f32 = 7.0;
const BAR_MIN_SCALE: f32 = 0.85;
const BAR_TIGHT_SCALE: f32 = 0.8;
/// `--map-rail-inset` and `--map-trigger-clear`: where the bar's room ends.
const TRIGGER_CLEAR: f32 = 154.0;
/// `FLIGHT_BAR_SHARE`: how much of the map the shrinking bar may cover.
pub const SHRINK_SHARE: f32 = 0.5;

/// How `fitBar` sets the bar's buttons in the room it has.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Fit {
    pub width: f32,
    pub scale: f32,
    pub compact: bool,
}

/// `fitBar`: every button one width and one caption size, 72 px at full size
/// with a long caption set a little smaller; narrower rooms take them all
/// down together (`is-tight`); icons only when even that will not fit.
/// `longest` is the widest caption at full size, `others` what the bar holds
/// besides its buttons, `n` the buttons.
pub fn fit_bar(longest: f32, others: f32, n: f32, room: f32) -> Fit {
    let scale_at = |w: f32| ((w - 2.0 * BAR_EDGE) / longest.max(1.0) * 100.0).floor().min(100.0) / 100.0;
    let (mut width, mut scale) = (BTN_W, scale_at(BTN_W));
    if scale < BAR_MIN_SCALE {
        scale = BAR_MIN_SCALE;
        width = (longest * BAR_MIN_SCALE + 2.0 * BAR_EDGE).ceil();
    }
    if others + n * width <= room {
        return Fit {
            width,
            scale,
            compact: false,
        };
    }
    let mut narrow = ((room - others) / n).floor();
    for _ in 0..4 {
        let fits = scale_at(narrow);
        if fits < BAR_TIGHT_SCALE {
            break;
        }
        if others + n * narrow <= room {
            return Fit {
                width: narrow,
                scale: fits,
                compact: false,
            };
        }
        narrow -= 1.0;
    }
    Fit {
        width: 35.0,
        scale: 1.0,
        compact: true,
    }
}
const TAKEOFF_MIN_M: f64 = 1.0;
const TAKEOFF_MAX_M: f64 = 120.0;
const TAKEOFF_SPAN: f64 = 1000.0;
const ALTITUDE_COARSE_FROM_M: f64 = 50.0;
const ALTITUDE_COARSE_STEP_M: f64 = 5.0;
const AIRBORNE_LANDED: [i64; 3] = [2, 3, 4];
const LANDED_ON_GROUND: i64 = 1;
const AIRBORNE_FALLBACK_M: f64 = 1.5;

/// "air", "ground" or "" (unknown, or no link), as app.js flightPhase.
pub fn flight_phase(s: &Value) -> &'static str {
    if s.get("connected") != Some(&Value::Bool(true)) {
        return "";
    }
    let landed = s.get("landed_state").and_then(Value::as_f64).map(|v| v as i64);
    if landed.is_some_and(|l| AIRBORNE_LANDED.contains(&l)) {
        return "air";
    }
    let armed = s.get("armed").is_some_and(|v| v.as_bool() == Some(true));
    if !armed || landed == Some(LANDED_ON_GROUND) {
        return "ground";
    }
    let agl = s.get("altitude_agl").and_then(Value::as_f64).unwrap_or(f64::NAN);
    if agl > AIRBORNE_FALLBACK_M {
        "air"
    } else {
        ""
    }
}

/// The two buttons when TAKEOFF and LAND stand apart: in the air TAKEOFF is
/// greyed, on the ground LAND is. Each is its tooltip and whether it works.
pub fn split_buttons(phase: &str) -> ((&'static str, bool), (&'static str, bool)) {
    (
        if phase == "air" {
            ("Takeoff: already in the air", false)
        } else {
            ("Takeoff", true)
        },
        if phase == "ground" {
            ("Land: on the ground", false)
        } else {
            ("Land", true)
        },
    )
}

/// What the single TAKEOFF / LAND button does: LAND in the air, and for an
/// armed vehicle whose phase cannot be told; a takeoff card over something
/// that may be hovering is not harmless, a land on the ground is.
pub fn single_button_role(s: &Value) -> &'static str {
    let phase = flight_phase(s);
    let connected = s.get("connected") == Some(&Value::Bool(true));
    let armed = s.get("armed") == Some(&Value::Bool(true));
    if phase == "air" || (phase.is_empty() && connected && armed) {
        "land"
    } else {
        "takeoff"
    }
}

pub fn is_ground(s: &Value) -> bool {
    crate::data::capabilities::is_ground(s)
}

/// The slider is logarithmic: a fixed distance along it is the same share of
/// height, so the low heights, where a metre matters, get the room.
pub fn altitude_to_position(metres: f64, min: f64, max: f64, span: f64) -> f64 {
    let m = metres.clamp(min, max);
    js_round(span * (m / min).ln() / (max / min).ln())
}

/// Whole metres up to 50 m, 5 m above, because a thumb cannot tell 83 m
/// from 84 m.
pub fn position_to_altitude(position: f64, min: f64, max: f64, span: f64) -> f64 {
    let raw = min * (max / min).powf(position / span);
    let step = if raw >= ALTITUDE_COARSE_FROM_M {
        ALTITUDE_COARSE_STEP_M
    } else {
        1.0
    };
    (js_round(raw / step) * step).clamp(min, max)
}

/// The metres a typed altitude stands for, or None while the box holds
/// nothing usable; clamped to the slider's range and rounded to the
/// centimetre.
pub fn altitude_from_entry(text: &str, to_si: impl Fn(f64) -> f64, min: f64, max: f64) -> Option<f64> {
    let typed = text.trim();
    if typed.is_empty() {
        return None;
    }
    let value: f64 = typed.parse().ok()?;
    if !value.is_finite() {
        return None;
    }
    Some(js_round(to_si(value).clamp(min, max) * 100.0) / 100.0)
}

/// One mode the picker offers.
#[derive(Debug, Clone, PartialEq)]
pub struct Mode {
    pub value: String,
    pub label: String,
}

/// The picker's rows from `/api/mavlink/modes`, labels from its `labels`.
pub fn modes_from(data: &Value) -> Vec<Mode> {
    let labels = data.get("labels");
    data.get("modes")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(Value::as_str)
                .map(|m| Mode {
                    value: m.to_string(),
                    label: labels
                        .and_then(|l| l.get(m))
                        .and_then(Value::as_str)
                        .unwrap_or(m)
                        .to_string(),
                })
                .collect()
        })
        .unwrap_or_default()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Command {
    Arm,
    Takeoff,
    Land,
    Rtl,
    Mode,
    /// FLY of the plan card.
    Plan,
    /// The map menu's "Fly to this point".
    Goto,
    /// The map menu's "Set home here".
    SetHome,
}

/// What the command threads leave for the bar and the board.
#[derive(Debug, Default)]
struct Outcome {
    busy: Vec<Command>,
    /// Commands sent, with the actions a vehicle line about them would name.
    begun: Vec<(u64, Vec<&'static str>)>,
    /// Their answers: Ok, or the reason to put on the board.
    settled: Vec<(u64, Result<(), String>)>,
    /// Errors that are no command's (the mode list did not load).
    plain: Vec<String>,
}

/// The bar's state.
pub struct FlightBar {
    server: Server,
    /// Where the bar ended last frame, in the map's own coordinates: an
    /// unmoved checklist window starts 10 px under it.
    pub bar_bottom: f32,
    /// `ui.takeoff_land_split`: TAKEOFF and LAND as two buttons.
    pub split: bool,
    /// `ui.flight_bar_shrink`: narrow the bar as soon as it would cover more
    /// than half the map.
    pub shrink: bool,
    modes: Arc<Mutex<Vec<Mode>>>,
    outcome: Arc<Mutex<Outcome>>,
    takeoff_open: bool,
    takeoff_alt: f64,
    takeoff_text: String,
    mode_open: bool,
    was_connected: bool,
    next_token: u64,
    attempts: std::collections::HashMap<u64, u64>,
    /// The token of the plan sent, and whether the vehicle took it.
    plan_token: Option<u64>,
    plan_done: bool,
}

impl FlightBar {
    pub fn new(server: Server, ctx: &egui::Context) -> FlightBar {
        let bar = FlightBar {
            server,
            bar_bottom: 0.0,
            split: false,
            shrink: false,
            modes: Arc::default(),
            outcome: Arc::default(),
            takeoff_open: false,
            takeoff_alt: 10.0,
            takeoff_text: String::new(),
            mode_open: false,
            was_connected: false,
            next_token: 0,
            attempts: std::collections::HashMap::new(),
            plan_token: None,
            plan_done: false,
        };
        bar.load_modes(ctx);
        bar
    }

    /// Fetches the mode list on a thread; the picker fills when it lands.
    fn load_modes(&self, ctx: &egui::Context) {
        let server = self.server.clone();
        let modes = self.modes.clone();
        let outcome = self.outcome.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            match http::get_json(&server, "/api/mavlink/modes") {
                Ok(data) => *modes.relock() = modes_from(&data),
                Err(e) => outcome.relock().plain.push(if e.is_empty() {
                    "Could not load flight modes".into()
                } else {
                    e
                }),
            }
            ctx.request_repaint();
        });
    }

    fn busy(&self, c: Command) -> bool {
        self.outcome.lock().map(|o| o.busy.contains(&c)).unwrap_or(false)
    }

    /// POSTs a command the way Corvus.telemetry.postAction does: an answer
    /// without `ok: true` is a refusal, its `error` the reason.
    fn send(
        &mut self,
        ctx: &egui::Context,
        command: Command,
        actions: &[&'static str],
        path: &'static str,
        body: Value,
        fallback: String,
    ) {
        self.next_token += 1;
        let token = self.next_token;
        {
            let mut o = self.outcome.relock();
            o.busy.push(command);
            o.begun.push((token, actions.to_vec()));
        }
        let server = self.server.clone();
        let outcome = self.outcome.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let result = http::post_json(&server, path, &body).and_then(|data| {
                if data.get("ok") == Some(&Value::Bool(true)) {
                    Ok(())
                } else {
                    Err(data
                        .get("error")
                        .and_then(Value::as_str)
                        .unwrap_or("Command rejected by vehicle")
                        .to_string())
                }
            });
            let mut o = outcome.relock();
            o.busy.retain(|c| *c != command);
            o.settled
                .push((token, result.map_err(|e| if e.is_empty() { fallback } else { e })));
            drop(o);
            ctx.request_repaint();
        });
    }

    /// Hands what the commands did to the board, as app.js does with
    /// Corvus.topbar: beginCommand when sent, succeedCommand on an answer,
    /// failCommand and notifyError on a refusal.
    pub fn settle(&mut self, board: &mut crate::notify::Board, now: f64) {
        let (begun, settled, plain) = {
            let mut o = self.outcome.relock();
            (
                std::mem::take(&mut o.begun),
                std::mem::take(&mut o.settled),
                std::mem::take(&mut o.plain),
            )
        };
        for (token, actions) in begun {
            let attempt = board.begin(&actions, now);
            self.attempts.insert(token, attempt);
        }
        for (token, result) in settled {
            if self.plan_token == Some(token) {
                self.plan_token = None;
                self.plan_done = result.is_ok();
            }
            let Some(attempt) = self.attempts.remove(&token) else {
                continue;
            };
            match result {
                Ok(()) => board.succeeded(attempt),
                Err(e) => board.failed(attempt, &e, now),
            }
        }
        for e in plain {
            board.add_local("critical", &e, now);
        }
    }

    /// Draws the bar and its card inside `area` (the map).
    pub fn show(&mut self, ui: &mut egui::Ui, area: Rect, s: &Value, units: &Units, tools: &mut Tools) {
        let ctx = ui.ctx().clone();
        let connected = s.get("connected") == Some(&Value::Bool(true));
        if connected && !self.was_connected {
            self.load_modes(&ctx);
        }
        self.was_connected = connected;
        let armed = s.get("armed") == Some(&Value::Bool(true));
        let ground = is_ground(s);
        let role = single_button_role(s);
        // Single: the one button turns into LAND in the air and the card is
        // closed with it. Split: TAKEOFF is greyed in the air.
        if (!self.split && role == "land") || ground || (self.split && flight_phase(s) == "air") {
            self.takeoff_open = false;
        }

        let caption_h = style::galley(ui, "ARM", 10.0, Weight::W600, style::text_2(), 0.6)
            .size()
            .y;
        let btn_h = 1.0 + 9.0 + 17.0 + 3.0 + caption_h + 9.0 + 1.0;
        let modes = self.modes.lock().map(|m| m.clone()).unwrap_or_default();
        let current = s.get("mode").and_then(Value::as_str).unwrap_or("");
        let current_label = s
            .get("mode_label")
            .and_then(Value::as_str)
            .filter(|l| !l.is_empty())
            .unwrap_or(current);
        let picked = modes.iter().find(|m| m.value == current).map(|m| m.label.clone());
        let select_text = match (&picked, current.is_empty()) {
            (Some(l), _) => l.clone(),
            (None, false) => current_label.to_string(),
            (None, true) => "SELECT MODE".into(),
        };
        let select_g = style::galley(ui, &select_text, 11.0, Weight::W600, style::text_1(), 0.5);
        let select_w = (10.0 + select_g.size().x + 26.0 + 2.0).max(120.0);
        let select_h = (2.0f32 + 8.0 + 11.0 * 1.21 + 8.0).max(if crate::app::touch::on() { 54.0 } else { 0.0 });

        let mut buttons: Vec<(Command, &'static str, &str, &str, bool)> = vec![(
            Command::Arm,
            "power",
            if armed { "DISARM" } else { "ARM" },
            "Arm / Disarm",
            true,
        )];
        if !ground && self.split {
            let ((tt, te), (lt, le)) = split_buttons(flight_phase(s));
            buttons.push((Command::Takeoff, "arrow-up", "TAKEOFF", tt, te));
            buttons.push((Command::Land, "arrow-down", "LAND", lt, le));
        } else if !ground {
            let land = role == "land";
            buttons.push((
                if land { Command::Land } else { Command::Takeoff },
                if land { "arrow-down" } else { "arrow-up" },
                if land { "LAND" } else { "TAKEOFF" },
                if land { "Land" } else { "Takeoff" },
                true,
            ));
        }
        buttons.push((
            Command::Rtl,
            "house",
            "RETURN",
            if ground { "Return to start" } else { "Return to Launch" },
            true,
        ));
        let n = buttons.len() as f32 + 1.0;
        // the room the bar sits in ends where the map's own controls begin
        let room = if self.shrink {
            area.width() * SHRINK_SHARE
        } else {
            area.width() - INSET - TRIGGER_CLEAR
        };
        let mut longest = 0.0f32;
        for c in ["DISARM", "TAKEOFF", "LAND", "RETURN", "PLAN"] {
            let g = style::galley(ui, c, 10.0, Weight::W600, style::text_2(), 0.6);
            longest = longest.max(g.size().x);
        }
        let fixed = BAR_PAD * 2.0 + n * BAR_GAP + 5.0 + BAR_GAP + select_w + 2.0;
        let fit = fit_bar(longest, fixed, n, room);
        let (btn_w, btn_h) = if fit.compact {
            (fit.width, 1.0 + 9.0 + 17.0 + 9.0 + 1.0)
        } else {
            (fit.width, btn_h)
        };
        // Tablet mode: the tallest targets in the app (`.fa-btn` min 80 by 54,
        // 54 wide when the bar is narrowed).
        let (btn_w, btn_h) = if crate::app::touch::on() {
            (
                btn_w.max(if fit.compact || fit.width < BTN_W { 54.0 } else { 80.0 }),
                btn_h.max(54.0),
            )
        } else {
            (btn_w, btn_h)
        };
        let select_w = if fit.compact { (select_w).max(96.0) } else { select_w };
        let bar_w = BAR_PAD * 2.0 + n * btn_w + n * BAR_GAP + 5.0 + BAR_GAP + select_w + 2.0;
        let bar_h = BAR_PAD * 2.0 + btn_h + 2.0;
        let bar = Rect::from_min_size(area.min + Vec2::splat(INSET), Vec2::new(bar_w, bar_h));
        self.bar_bottom = bar.bottom() - area.top();
        let painter = ui.painter().clone();
        style::glass_panel(&painter, "flightbar", bar, style::RADIUS, 0.6);
        ui.interact(bar, ui.scope_id().with("flightbar"), egui::Sense::click_and_drag());

        let mut x = bar.left() + BAR_PAD + 1.0;
        let y = bar.top() + BAR_PAD + 1.0;
        let mut takeoff_anchor = None;
        for (cmd, icon, caption, title, enabled) in &buttons {
            let r = Rect::from_min_size(Pos2::new(x, y), Vec2::new(btn_w, btn_h));
            let busy = self.busy(*cmd);
            let clicked = fa_button(
                ui,
                r,
                icon,
                caption,
                title,
                *enabled,
                busy,
                if *cmd == Command::Arm && armed {
                    Lit::Armed
                } else {
                    Lit::Off
                },
                (fit.scale, fit.compact),
            );
            if clicked && !busy {
                match cmd {
                    Command::Arm => {
                        let will = !armed;
                        let fallback = if will { "Arm failed" } else { "Disarm failed" };
                        self.send(
                            &ctx,
                            Command::Arm,
                            &[if will { "arm" } else { "disarm" }],
                            "/api/mavlink/arm",
                            json!({"arm": will}),
                            fallback.into(),
                        );
                    }
                    Command::Takeoff => {
                        self.takeoff_open = !self.takeoff_open;
                        self.takeoff_text = units.format_length(self.takeoff_alt, 0, false, true);
                        self.mode_open = false;
                    }
                    Command::Land => self.send(
                        &ctx,
                        Command::Land,
                        &["land"],
                        "/api/mavlink/land",
                        json!({}),
                        "Land rejected".into(),
                    ),
                    Command::Rtl => self.send(
                        &ctx,
                        Command::Rtl,
                        &["rtl"],
                        "/api/mavlink/rtl",
                        json!({}),
                        "Return rejected".into(),
                    ),
                    Command::Mode | Command::Plan | Command::Goto | Command::SetHome => {}
                }
            }
            if *cmd == Command::Takeoff || (*cmd == Command::Land && !self.split) {
                takeoff_anchor = Some(r);
            }
            x += btn_w + BAR_GAP;
        }
        // PLAN: click the map to add points, FLY sends them.
        let plan = Rect::from_min_size(Pos2::new(x, y), Vec2::new(btn_w, btn_h));
        if !connected && tools.plan.active {
            tools.plan.set_active(false);
        }
        let plan_title = if ground {
            "Plan drive-to-points route"
        } else {
            "Plan fly-to-points mission"
        };
        if fa_button(
            ui,
            plan,
            "route",
            "PLAN",
            plan_title,
            connected,
            false,
            if tools.plan.active { Lit::Plan } else { Lit::Off },
            (fit.scale, fit.compact),
        ) {
            let on = !tools.plan.active;
            tools.plan.set_active(on);
            self.takeoff_open = false;
            self.mode_open = false;
        }
        let plan_anchor = plan;
        x += btn_w + BAR_GAP;
        // .fa-divider: 1 x 34, margin 0 2
        painter.vline(
            x + 2.0,
            egui::Rangef::new(bar.center().y - 17.0, bar.center().y + 17.0),
            Stroke::new(1.0, style::border_soft()),
        );
        x += 5.0 + BAR_GAP;
        let select = Rect::from_min_size(
            Pos2::new(x, bar.center().y - select_h / 2.0),
            Vec2::new(select_w, select_h),
        );
        self.mode_picker(
            ui,
            &ctx,
            select,
            select_g,
            &modes,
            current,
            current_label,
            picked.is_none() && current.is_empty(),
            ground,
        );

        if std::mem::take(&mut self.plan_done) {
            tools.plan.set_active(false);
            tools.plan.clear();
            tools.plan.panel = false;
        }
        if self.takeoff_open {
            if let Some(anchor) = takeoff_anchor {
                self.plan_card_closed(tools);
                self.takeoff_card(ui, &ctx, area, anchor, units);
            }
        }
        if tools.plan.panel && !self.takeoff_open {
            self.plan_card(ui, &ctx, area, plan_anchor, units, tools, s, ground);
        }
    }

    /// The two cards never show together.
    fn plan_card_closed(&mut self, tools: &mut Tools) {
        tools.plan.panel = false;
    }

    /// The plan card: hangs 16 px under PLAN, 260 px wide. Closing it keeps
    /// the points; FLY drops the plan once the vehicle accepts it.
    #[allow(clippy::too_many_arguments)]
    fn plan_card(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        area: Rect,
        anchor: Rect,
        units: &Units,
        tools: &mut Tools,
        state: &Value,
        ground: bool,
    ) {
        let w = 260.0;
        let title_text = if ground { "DRIVE TO POINTS" } else { "FLY TO POINTS" };
        let title = style::galley(ui, title_text, 11.0, Weight::W600, style::text_2(), 1.0);
        let count_h = 7.0 * 2.0 + 15.0 * 1.21 + 2.0;
        let slider_h = 30.0;
        let actions_h = 10.0 * 2.0 + 15.0 + 2.0;
        let h = 14.0 + 20.0 + 12.0 + count_h + 12.0 + if ground { 0.0 } else { slider_h + 12.0 } + actions_h + 14.0;
        let left = anchor.left().min(area.right() - w - 14.0).max(area.left());
        let card = Rect::from_min_size(Pos2::new(left, anchor.bottom() + 1.0 + BAR_PAD + 16.0), Vec2::new(w, h));
        let mut fly = false;
        let mut cleared = false;
        let mut closed = false;
        egui::Area::new(ui.scope_id().with("plancard"))
            .order(egui::Order::Middle)
            .fixed_pos(card.min)
            .show(ctx, |ui| {
                let painter = ui.painter().clone();
                style::glass_panel(&painter, "plan", card, style::RADIUS, 0.6);
                ui.allocate_rect(card, egui::Sense::click_and_drag());
                let inner = card.shrink(14.0);
                painter.galley(
                    Pos2::new(inner.left(), inner.top() + 10.0 - title.size().y / 2.0),
                    title,
                    style::text_2(),
                );
                let close =
                    Rect::from_center_size(Pos2::new(inner.right() - 10.0, inner.top() + 10.0), Vec2::splat(20.0));
                let resp = ui.interact(close, ui.scope_id().with("planclose"), egui::Sense::click());
                if resp.hovered() {
                    painter.rect_filled(close, 5, style::surface_hover());
                }
                icons::paint(
                    ui,
                    "x",
                    close.shrink(3.0),
                    if resp.hovered() {
                        style::text_1()
                    } else {
                        style::text_3()
                    },
                );
                if resp.on_hover_tr("Close").clicked() {
                    closed = true;
                }

                // POINTS n
                let mut y = inner.top() + 20.0 + 12.0;
                let count = Rect::from_min_size(Pos2::new(inner.left(), y), Vec2::new(inner.width(), count_h));
                painter.rect_filled(count, style::RADIUS_BTN, style::surface_2());
                painter.rect_stroke(
                    count,
                    style::RADIUS_BTN,
                    Stroke::new(1.0, style::border_soft()),
                    egui::StrokeKind::Inside,
                );
                let n = tools.plan.points.len();
                let label = style::galley(ui, "POINTS", 10.0, Weight::W600, style::text_3(), 0.9);
                let value = style::galley(
                    ui,
                    &tools.plan.count_text(),
                    15.0,
                    Weight::W700,
                    if n == 0 {
                        style::text_3()
                    } else {
                        style::accent_bright()
                    },
                    0.0,
                );
                let base = count.center().y + value.size().y / 2.0 - 3.0;
                painter.galley(
                    Pos2::new(count.left() + 10.0, base - label.size().y + 1.0),
                    label,
                    style::text_3(),
                );
                painter.galley(
                    Pos2::new(
                        count.right() - 10.0 - value.size().x,
                        count.center().y - value.size().y / 2.0,
                    ),
                    value,
                    if n == 0 {
                        style::text_3()
                    } else {
                        style::accent_bright()
                    },
                );
                y += count_h + 12.0;

                if !ground {
                    let alt_text = format!("{} AGL", units.format_length(tools.plan.alt_agl(), 0, false, false));
                    let value = style::galley(ui, &alt_text, 16.0, Weight::W600, style::healthy(), 0.0);
                    let vw = value.size().x.max(48.0);
                    let row_y = y + slider_h / 2.0;
                    let track = egui::Rangef::new(inner.left(), inner.right() - 10.0 - vw);
                    let span = crate::map::tools::ALT_MAX_M - crate::map::tools::ALT_MIN_M;
                    let t = ((tools.plan.alt_agl() - crate::map::tools::ALT_MIN_M) / span) as f32;
                    let thumb_x = track.min + (track.max - track.min) * t;
                    let track_rect = Rect::from_x_y_ranges(track, egui::Rangef::new(row_y - 2.0, row_y + 2.0));
                    painter.rect_filled(track_rect, 2, style::surface_3());
                    let hit = Rect::from_x_y_ranges(track, egui::Rangef::new(row_y - 9.0, row_y + 9.0));
                    let slide = ui.interact(hit, ui.scope_id().with("planslider"), egui::Sense::click_and_drag());
                    if let Some(p) = slide
                        .interact_pointer_pos()
                        .filter(|_| slide.dragged() || slide.clicked())
                    {
                        let t = f64::from(((p.x - track.min) / (track.max - track.min)).clamp(0.0, 1.0));
                        tools.plan.alt = (crate::map::tools::ALT_MIN_M + t * span).round();
                    }
                    painter.circle(
                        Pos2::new(thumb_x, row_y),
                        8.0,
                        style::accent(),
                        Stroke::new(2.0, style::text_1()),
                    );
                    painter.galley(
                        Pos2::new(inner.right() - value.size().x, row_y - value.size().y / 2.0),
                        value,
                        style::healthy(),
                    );
                    y += slider_h + 12.0;
                }

                // FLY and CLEAR
                let clear_label = style::galley(ui, "CLEAR", 12.0, Weight::W600, style::text_1(), 0.5);
                let clear_w = 10.0 * 2.0 + 14.0 + 6.0 + clear_label.size().x + 2.0;
                let fly_rect = Rect::from_min_size(
                    Pos2::new(inner.left(), y),
                    Vec2::new(inner.width() - 8.0 - clear_w, actions_h),
                );
                let clear_rect =
                    Rect::from_min_size(Pos2::new(fly_rect.right() + 8.0, y), Vec2::new(clear_w, actions_h));
                let ready = tools.plan.fly_ready(state);
                let busy = self.busy(Command::Plan);
                let fly_resp = ui.interact(
                    fly_rect,
                    ui.scope_id().with("planfly"),
                    if ready {
                        egui::Sense::click()
                    } else {
                        egui::Sense::hover()
                    },
                );
                let fill = if ready && fly_resp.hovered() {
                    style::accent_hover()
                } else {
                    style::accent()
                };
                let alpha = if !ready {
                    0.4
                } else if busy {
                    0.5
                } else {
                    1.0
                };
                painter.rect_filled(fly_rect, style::RADIUS_BTN, fill.gamma_multiply(alpha));
                let fly_text = style::galley(
                    ui,
                    if ground { "DRIVE" } else { "FLY" },
                    12.0,
                    Weight::W700,
                    style::text_inverted(),
                    0.6,
                );
                let total = 15.0 + 6.0 + fly_text.size().x;
                let start = fly_rect.center().x - total / 2.0;
                icons::paint(
                    ui,
                    "send",
                    Rect::from_min_size(Pos2::new(start, fly_rect.center().y - 7.5), Vec2::splat(15.0)),
                    style::text_inverted().gamma_multiply(alpha),
                );
                painter.galley(
                    Pos2::new(start + 21.0, fly_rect.center().y - fly_text.size().y / 2.0),
                    fly_text,
                    style::text_inverted().gamma_multiply(alpha),
                );
                if fly_resp.clicked() && ready && !busy {
                    fly = true;
                }

                let clear_resp = ui.interact(clear_rect, ui.scope_id().with("planclear"), egui::Sense::click());
                painter.rect_filled(
                    clear_rect,
                    style::RADIUS_BTN,
                    if clear_resp.hovered() {
                        style::surface_hover()
                    } else {
                        style::surface_2()
                    },
                );
                painter.rect_stroke(
                    clear_rect,
                    style::RADIUS_BTN,
                    Stroke::new(1.0, style::border()),
                    egui::StrokeKind::Inside,
                );
                let total = 14.0 + 6.0 + clear_label.size().x;
                let start = clear_rect.center().x - total / 2.0;
                icons::paint(
                    ui,
                    "rotate-ccw",
                    Rect::from_min_size(Pos2::new(start, clear_rect.center().y - 7.0), Vec2::splat(14.0)),
                    style::text_1(),
                );
                painter.galley(
                    Pos2::new(start + 20.0, clear_rect.center().y - clear_label.size().y / 2.0),
                    clear_label,
                    style::text_1(),
                );
                if clear_resp.clicked() {
                    cleared = true;
                }
            });
        if closed {
            tools.plan.panel = false;
        }
        if cleared {
            tools.plan.clear();
        }
        if fly {
            let body = tools.plan.body(ground);
            let (label, fallback) = if ground {
                ("gotopoints", "Drive to points rejected")
            } else {
                ("gotopoints", "Fly to points rejected")
            };
            self.send(
                ctx,
                Command::Plan,
                &[label],
                "/api/mavlink/gotopoints",
                body,
                fallback.into(),
            );
            self.plan_token = Some(self.next_token);
        }
    }

    /// A row of the map menu was chosen.
    pub fn run_action(&mut self, ctx: &egui::Context, action: Action, ground: bool, plan_alt: f64) {
        match action {
            Action::Goto(p) => {
                let point = if ground {
                    json!({"lat": p[1], "lon": p[0]})
                } else {
                    json!({"lat": p[1], "lon": p[0], "alt_agl": plan_alt})
                };
                self.send(
                    ctx,
                    Command::Goto,
                    &["gotopoints"],
                    "/api/mavlink/gotopoints",
                    json!({"points": [point]}),
                    if ground {
                        "Drive to point rejected"
                    } else {
                        "Fly to point rejected"
                    }
                    .into(),
                );
            }
            Action::SetHome(p) => self.send(
                ctx,
                Command::SetHome,
                &["sethome"],
                "/api/mavlink/sethome",
                json!({"lat": p[1], "lon": p[0]}),
                "Set home rejected".into(),
            ),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn mode_picker(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        rect: Rect,
        text: Arc<egui::Galley>,
        modes: &[Mode],
        current: &str,
        current_label: &str,
        placeholder: bool,
        ground: bool,
    ) {
        let busy = self.busy(Command::Mode);
        let response = ui.interact(rect, ui.scope_id().with("modesel"), egui::Sense::click());
        let response = response.on_hover_tr(if ground { "Drive Mode" } else { "Flight Mode" });
        let fill = if response.hovered() && !busy {
            style::tint(0.10)
        } else {
            style::tint(0.06)
        };
        let painter = ui.painter();
        let alpha = if busy { 0.45 } else { 1.0 };
        let stroke = if self.mode_open {
            Stroke::new(1.0, style::accent())
        } else {
            Stroke::NONE
        };
        if self.mode_open {
            painter.rect_stroke(
                rect.expand(2.0),
                style::RADIUS_BTN + 2,
                Stroke::new(2.0, style::accent_dim()),
                egui::StrokeKind::Inside,
            );
        }
        {
            let r = rect;
            painter.rect_filled(r, style::RADIUS_BTN, fill.gamma_multiply(alpha));
            painter.rect_stroke(r, style::RADIUS_BTN, stroke, egui::StrokeKind::Inside);
        }
        let color = if placeholder { style::text_3() } else { style::text_1() };
        painter.galley(
            Pos2::new(rect.left() + 11.0, rect.center().y - text.size().y / 2.0),
            text,
            color.gamma_multiply(alpha),
        );
        let chevron = Rect::from_center_size(Pos2::new(rect.right() - 8.0 - 6.0, rect.center().y), Vec2::splat(12.0));
        icons::paint(ui, "chevron-down", chevron, style::text_3());
        if response.clicked() && !busy {
            self.mode_open = !self.mode_open;
            self.takeoff_open = false;
        }
        if !self.mode_open {
            return;
        }
        // The list: .ui-menu on .glass, rows .option-item.
        let mut rows: Vec<(Mode, bool)> = modes.iter().map(|m| (m.clone(), true)).collect();
        if !current.is_empty() && !modes.iter().any(|m| m.value == current) {
            rows.push((
                Mode {
                    value: current.into(),
                    label: current_label.into(),
                },
                false,
            ));
        }
        let items: Vec<menu::Row> = rows
            .iter()
            .map(|(m, enabled)| {
                let r = menu::Row::new(m.value.clone(), m.label.clone());
                if *enabled {
                    r
                } else {
                    r.disabled()
                }
            })
            .collect();
        let out = menu::dropdown(ui, "modemenu", rect, &items, current, &menu::Look::SELECT);
        if out.dismissed {
            self.mode_open = false;
        }
        let chosen = out.picked;
        if let Some(mode) = chosen {
            self.mode_open = false;
            if mode != current {
                let fallback = format!("Mode {mode} rejected");
                self.send(
                    ctx,
                    Command::Mode,
                    &["mode"],
                    "/api/mavlink/mode",
                    json!({"mode": mode}),
                    fallback,
                );
            }
        }
    }

    /// The takeoff card: hangs 16 px under its button, 260 px wide.
    fn takeoff_card(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, area: Rect, anchor: Rect, units: &Units) {
        let w = 260.0;
        let title = style::galley(ui, "TAKEOFF ALTITUDE", 11.0, Weight::W600, style::text_2(), 1.0);
        let confirm_h = 10.0 * 2.0 + 15.0;
        let h = 14.0 + 20.0 + 12.0 + 30.0 + 12.0 + confirm_h + 14.0;
        let left = anchor.left().min(area.right() - w - 14.0).max(area.left());
        let card = Rect::from_min_size(Pos2::new(left, anchor.bottom() + 1.0 + BAR_PAD + 16.0), Vec2::new(w, h));
        let mut confirm = false;
        egui::Area::new(ui.scope_id().with("takeoffcard"))
            .order(egui::Order::Middle)
            .fixed_pos(card.min)
            .show(ctx, |ui| {
                let painter = ui.painter().clone();
                style::glass_panel(&painter, "takeoff", card, style::RADIUS, 0.6);
                ui.allocate_rect(card, egui::Sense::click_and_drag());
                let inner = card.shrink(14.0);
                painter.galley(
                    Pos2::new(inner.left(), inner.top() + 10.0 - title.size().y / 2.0),
                    title,
                    style::text_2(),
                );
                // .icon-btn with the x
                let close =
                    Rect::from_center_size(Pos2::new(inner.right() - 10.0, inner.top() + 10.0), Vec2::splat(20.0));
                let resp = ui.interact(close, ui.scope_id().with("takeoffclose"), egui::Sense::click());
                if resp.hovered() {
                    listrow::paint(&painter, close, 5, Mark::Hover);
                }
                icons::paint(
                    ui,
                    "x",
                    close.shrink(3.0),
                    if resp.hovered() {
                        style::text_1()
                    } else {
                        style::text_3()
                    },
                );
                if resp.clicked() {
                    self.takeoff_open = false;
                }

                // The slider row: the track takes what the entry leaves.
                let row_y = inner.top() + 20.0 + 12.0 + 15.0;
                let unit_w = 18.0f32.max(
                    style::galley(ui, units.length.symbol, 12.0, Weight::W600, style::text_2(), 0.0)
                        .size()
                        .x,
                );
                let entry_w = 64.0;
                let track = egui::Rangef::new(inner.left(), inner.right() - 10.0 - entry_w - 4.0 - unit_w);
                let pos = altitude_to_position(self.takeoff_alt, TAKEOFF_MIN_M, TAKEOFF_MAX_M, TAKEOFF_SPAN);
                let thumb_x = track.min + (track.max - track.min) * (pos / TAKEOFF_SPAN) as f32;
                let track_rect = Rect::from_x_y_ranges(track, egui::Rangef::new(row_y - 2.0, row_y + 2.0));
                painter.rect_filled(track_rect, 2, style::surface_3());
                let hit = Rect::from_x_y_ranges(track, egui::Rangef::new(row_y - 9.0, row_y + 9.0));
                let slide = ui.interact(hit, ui.scope_id().with("takeoffslider"), egui::Sense::click_and_drag());
                if let Some(p) = slide
                    .interact_pointer_pos()
                    .filter(|_| slide.dragged() || slide.clicked())
                {
                    let t = ((p.x - track.min) / (track.max - track.min)).clamp(0.0, 1.0);
                    self.takeoff_alt =
                        position_to_altitude(f64::from(t) * TAKEOFF_SPAN, TAKEOFF_MIN_M, TAKEOFF_MAX_M, TAKEOFF_SPAN);
                    self.takeoff_text = units.format_length(self.takeoff_alt, 0, false, true);
                }
                painter.circle(
                    Pos2::new(thumb_x, row_y),
                    8.0,
                    style::accent(),
                    Stroke::new(2.0, style::text_1()),
                );

                // The entry: 64 px, 15 px semibold green, right aligned.
                let entry = Rect::from_min_size(Pos2::new(track.max + 10.0, row_y - 15.0), Vec2::new(entry_w, 30.0));
                let typed = TextField::new(&mut self.takeoff_text)
                    .font(15.0, Weight::W600)
                    .color(style::healthy())
                    .focus_color(style::healthy())
                    .pad(6.0, 6.0)
                    .align_right()
                    .show(ui, entry, "takeoffentry");
                if typed.changed() {
                    if let Some(m) = altitude_from_entry(
                        &self.takeoff_text,
                        |v| v / units.length.per_si,
                        TAKEOFF_MIN_M,
                        TAKEOFF_MAX_M,
                    ) {
                        self.takeoff_alt = m;
                    }
                }
                if typed.lost_focus() {
                    if ui.input(|i| i.key_pressed(egui::Key::Enter))
                        && altitude_from_entry(&self.takeoff_text, |v| v, f64::MIN, f64::MAX).is_some()
                    {
                        confirm = true;
                    }
                    self.takeoff_text = units.format_length(self.takeoff_alt, 0, false, true);
                }
                let unit = style::galley(ui, units.length.symbol, 12.0, Weight::W600, style::text_2(), 0.0);
                painter.galley(
                    Pos2::new(entry.right() + 4.0, row_y - unit.size().y / 2.0 + 1.0),
                    unit,
                    style::text_2(),
                );

                // .takeoff-confirm
                let button = Rect::from_min_size(
                    Pos2::new(inner.left(), inner.bottom() - confirm_h),
                    Vec2::new(inner.width(), confirm_h),
                );
                let busy = self.busy(Command::Takeoff);
                let resp = ui.interact(button, ui.scope_id().with("takeoffconfirm"), egui::Sense::click());
                let fill = if resp.hovered() {
                    style::healthy_bright()
                } else {
                    style::healthy()
                };
                let alpha = if busy { 0.45 } else { 1.0 };
                painter.rect_filled(button, style::RADIUS_BTN, fill.gamma_multiply(alpha));
                let label = style::galley(ui, "CONFIRM TAKEOFF", 12.0, Weight::W700, style::text_inverted(), 0.6);
                let total = 15.0 + 6.0 + label.size().x;
                let start = button.center().x - total / 2.0;
                icons::paint(
                    ui,
                    "arrow-up",
                    Rect::from_min_size(Pos2::new(start, button.center().y - 7.5), Vec2::splat(15.0)),
                    style::text_inverted(),
                );
                painter.galley(
                    Pos2::new(start + 21.0, button.center().y - label.size().y / 2.0),
                    label,
                    style::text_inverted(),
                );
                if resp.clicked() && !busy {
                    confirm = true;
                }
            });
        if confirm {
            self.takeoff_open = false;
            let alt = self.takeoff_alt;
            self.send(
                ctx,
                Command::Takeoff,
                &["takeoff", "arm"],
                "/api/mavlink/takeoff",
                json!({"altitude": alt}),
                "Takeoff rejected by autopilot".into(),
            );
        }
    }
}

/// How a `.fa-btn` is filled: not at all, `.armed` green or `.plan.active`
/// accent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lit {
    Off,
    Armed,
    Plan,
}

/// One `.fa-btn` of the flight bar: icon over caption; hover lifts it,
/// `.armed` fills it green, disabled is 40 % and busy 50 % with a pulsing ring.
#[allow(clippy::too_many_arguments)]
fn fa_button(
    ui: &mut egui::Ui,
    r: Rect,
    icon: &'static str,
    caption: &str,
    title: &str,
    enabled: bool,
    busy: bool,
    lit: Lit,
    fit: (f32, bool),
) -> bool {
    let look = fabar::Look {
        armed: lit == Lit::Armed,
        active: lit == Lit::Plan,
        busy,
        disabled: !enabled,
        ..Default::default()
    };
    fabar::button(ui, r, icon, caption, title, look, fit, ("fa", caption))
}

#[cfg(test)]
mod tests {
    #[test]
    fn apart_takeoff_is_greyed_in_the_air_and_land_on_the_ground() {
        assert_eq!(
            split_buttons("air"),
            (("Takeoff: already in the air", false), ("Land", true))
        );
        assert_eq!(
            split_buttons("ground"),
            (("Takeoff", true), ("Land: on the ground", false))
        );
        // Unknown: both offered, as the web bar does.
        assert_eq!(split_buttons(""), (("Takeoff", true), ("Land", true)));
    }

    use super::*;

    #[test]
    fn a_bar_with_room_keeps_its_72_px_buttons() {
        let fit = fit_bar(49.0, 190.0, 5.0, 700.0);
        assert_eq!(
            fit,
            Fit {
                width: 72.0,
                scale: 1.0,
                compact: false
            }
        );
    }

    #[test]
    fn a_long_caption_is_set_smaller_before_the_buttons_widen() {
        let fit = fit_bar(60.0, 190.0, 5.0, 900.0);
        assert_eq!((fit.width, fit.scale), (72.0, 0.96));
        // below the least scale the buttons widen instead
        let wide = fit_bar(70.0, 190.0, 5.0, 900.0);
        assert_eq!((wide.width, wide.scale), (74.0, 0.85));
    }

    #[test]
    fn a_narrower_room_takes_every_button_down_together() {
        let fit = fit_bar(49.0, 190.0, 5.0, 520.0);
        assert_eq!((fit.width, fit.compact), (66.0, false));
    }

    #[test]
    fn a_room_that_cannot_hold_the_captions_leaves_icons_only() {
        let fit = fit_bar(49.0, 190.0, 5.0, 300.0);
        assert!(fit.compact);
    }

    #[test]
    fn phases_and_the_single_button() {
        assert_eq!(flight_phase(&json!({"connected": false})), "");
        assert_eq!(flight_phase(&json!({"connected": true, "landed_state": 2})), "air");
        assert_eq!(flight_phase(&json!({"connected": true, "armed": false})), "ground");
        assert_eq!(
            flight_phase(&json!({"connected": true, "armed": true, "altitude_agl": 3})),
            "air"
        );
        assert_eq!(
            flight_phase(&json!({"connected": true, "armed": true, "altitude_agl": 0.5})),
            ""
        );
        assert_eq!(single_button_role(&json!({"connected": true, "armed": true})), "land");
        assert_eq!(
            single_button_role(&json!({"connected": true, "armed": false})),
            "takeoff"
        );
        assert_eq!(single_button_role(&json!({"connected": false})), "takeoff");
        assert!(is_ground(&json!({"connected": true, "vehicle_type": "SURFACE_BOAT"})));
    }

    #[test]
    fn the_logarithmic_slider() {
        let (min, max, span) = (TAKEOFF_MIN_M, TAKEOFF_MAX_M, TAKEOFF_SPAN);
        assert_eq!(altitude_to_position(1.0, min, max, span), 0.0);
        assert_eq!(altitude_to_position(120.0, min, max, span), 1000.0);
        assert_eq!(altitude_to_position(10.0, min, max, span), 481.0);
        assert_eq!(position_to_altitude(481.0, min, max, span), 10.0);
        assert_eq!(position_to_altitude(900.0, min, max, span), 75.0);
        assert_eq!(position_to_altitude(-5.0, min, max, span), 1.0);
    }

    #[test]
    fn typed_altitudes() {
        let id = |v: f64| v;
        assert_eq!(altitude_from_entry("", id, 1.0, 120.0), None);
        assert_eq!(altitude_from_entry("abc", id, 1.0, 120.0), None);
        assert_eq!(altitude_from_entry(" 500 ", id, 1.0, 120.0), Some(120.0));
        assert_eq!(altitude_from_entry("100", |ft| ft * 0.3048, 1.0, 120.0), Some(30.48));
    }

    #[test]
    fn modes_with_their_labels() {
        let data = json!({"modes": ["POSCTL", "HOLD"], "labels": {"POSCTL": "POSITION"}});
        assert_eq!(
            modes_from(&data),
            vec![
                Mode {
                    value: "POSCTL".into(),
                    label: "POSITION".into()
                },
                Mode {
                    value: "HOLD".into(),
                    label: "HOLD".into()
                }
            ]
        );
        assert!(modes_from(&json!({})).is_empty());
    }
}
