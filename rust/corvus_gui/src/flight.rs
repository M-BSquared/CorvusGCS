//! The flight bar at the top left of the map, the takeoff card under it and
//! the mode picker, as the web map has them (index.html `#flightActions`,
//! main.css `.flight-actions`, src/js/app.js `initFlightActions`).
//!
//! The behaviour is app.js's: one TAKEOFF button that turns into LAND in the
//! air (and for an armed vehicle whose phase cannot be told), the takeoff
//! card with its logarithmic slider from 1 to 120 m, ARM that reads DISARM
//! once armed, and a mode list from `/api/mavlink/modes` that shows a mode
//! the vehicle reports but cannot be commanded into as a disabled row of its
//! own. Every command is a POST to the same endpoint the web button posts
//! to, on a thread of its own, and the button is busy until the answer.

use std::sync::{Arc, Mutex};

use eframe::egui::{self, Color32, Pos2, Rect, Stroke, Vec2};
use serde_json::{json, Value};

use crate::http::{self, Server};
use crate::icons;
use crate::style::{self, Weight};
use crate::units::{js_round, Units};

/// `--map-rail-inset`.
pub const INSET: f32 = 14.0;
const BAR_PAD: f32 = 7.0;
const BAR_GAP: f32 = 7.0;
/// `--fa-w`, every button one width at full size.
const BTN_W: f32 = 72.0;
const TAKEOFF_MIN_M: f64 = 1.0;
const TAKEOFF_MAX_M: f64 = 120.0;
const TAKEOFF_SPAN: f64 = 1000.0;
const ALTITUDE_COARSE_FROM_M: f64 = 50.0;
const ALTITUDE_COARSE_STEP_M: f64 = 5.0;
const AIRBORNE_LANDED: [i64; 3] = [2, 3, 4];
const LANDED_ON_GROUND: i64 = 1;
const AIRBORNE_FALLBACK_M: f64 = 1.5;
const GROUND_TYPES: [&str; 3] = ["GROUNDED_ROVER", "GROUND_ROVER", "SURFACE_BOAT"];

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

fn is_ground(s: &Value) -> bool {
    s.get("connected") == Some(&Value::Bool(true))
        && GROUND_TYPES.contains(&s.get("vehicle_type").and_then(Value::as_str).unwrap_or(""))
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
    modes: Arc<Mutex<Vec<Mode>>>,
    outcome: Arc<Mutex<Outcome>>,
    takeoff_open: bool,
    takeoff_alt: f64,
    takeoff_text: String,
    mode_open: bool,
    was_connected: bool,
    next_token: u64,
    attempts: std::collections::HashMap<u64, u64>,
}

impl FlightBar {
    pub fn new(server: Server, ctx: &egui::Context) -> FlightBar {
        let bar = FlightBar {
            server,
            modes: Arc::default(),
            outcome: Arc::default(),
            takeoff_open: false,
            takeoff_alt: 10.0,
            takeoff_text: String::new(),
            mode_open: false,
            was_connected: false,
            next_token: 0,
            attempts: std::collections::HashMap::new(),
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
                Ok(data) => *modes.lock().expect("modes") = modes_from(&data),
                Err(e) => outcome.lock().expect("outcome").plain.push(if e.is_empty() {
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
            let mut o = self.outcome.lock().expect("outcome");
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
            let mut o = outcome.lock().expect("outcome");
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
            let mut o = self.outcome.lock().expect("outcome");
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
    pub fn show(&mut self, ui: &mut egui::Ui, area: Rect, s: &Value, units: &Units) {
        let ctx = ui.ctx().clone();
        let connected = s.get("connected") == Some(&Value::Bool(true));
        if connected && !self.was_connected {
            self.load_modes(&ctx);
        }
        self.was_connected = connected;
        let armed = s.get("armed") == Some(&Value::Bool(true));
        let ground = is_ground(s);
        let role = single_button_role(s);
        if role == "land" || ground {
            self.takeoff_open = false;
        }

        let caption_h = style::galley(ui, "ARM", 10.0, Weight::W600, style::TEXT_2, 0.6)
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
        let select_g = style::galley(ui, &select_text, 11.0, Weight::W600, style::TEXT_1, 0.5);
        let select_w = (10.0 + select_g.size().x + 26.0 + 2.0).max(120.0);
        let select_h = 2.0 + 8.0 + 11.0 * 1.21 + 8.0;

        let mut buttons: Vec<(Command, &'static str, &str, &str, bool)> = vec![(
            Command::Arm,
            "power",
            if armed { "DISARM" } else { "ARM" },
            "Arm / Disarm",
            true,
        )];
        if !ground {
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
        let bar_w = BAR_PAD * 2.0 + n * BTN_W + n * BAR_GAP + 5.0 + BAR_GAP + select_w + 2.0;
        let bar_h = BAR_PAD * 2.0 + btn_h + 2.0;
        let bar = Rect::from_min_size(area.min + Vec2::splat(INSET), Vec2::new(bar_w, bar_h));
        let painter = ui.painter().clone();
        style::glass_panel(&painter, "flightbar", bar, style::RADIUS, 0.6);
        ui.interact(bar, ui.scope_id().with("flightbar"), egui::Sense::click_and_drag());

        let mut x = bar.left() + BAR_PAD + 1.0;
        let y = bar.top() + BAR_PAD + 1.0;
        let mut takeoff_anchor = None;
        for (cmd, icon, caption, title, enabled) in &buttons {
            let r = Rect::from_min_size(Pos2::new(x, y), Vec2::new(BTN_W, btn_h));
            let busy = self.busy(*cmd);
            let clicked = fa_button(
                ui,
                r,
                icon,
                caption,
                title,
                *enabled,
                busy,
                *cmd == Command::Arm && armed,
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
                    Command::Mode => {}
                }
            }
            if matches!(cmd, Command::Takeoff | Command::Land) {
                takeoff_anchor = Some(r);
            }
            x += BTN_W + BAR_GAP;
        }
        // PLAN: the fly-to-points planner comes with the planning step.
        let plan = Rect::from_min_size(Pos2::new(x, y), Vec2::new(BTN_W, btn_h));
        fa_button(
            ui,
            plan,
            "route",
            "PLAN",
            "Plan fly-to-points mission (not in this build yet)",
            false,
            false,
            false,
        );
        x += BTN_W + BAR_GAP;
        // .fa-divider: 1 x 34, margin 0 2
        painter.vline(
            x + 2.0,
            egui::Rangef::new(bar.center().y - 17.0, bar.center().y + 17.0),
            Stroke::new(1.0, style::BORDER_SOFT),
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

        if self.takeoff_open {
            if let Some(anchor) = takeoff_anchor {
                self.takeoff_card(ui, &ctx, area, anchor, units);
            }
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
        let response = response.on_hover_text(if ground { "Drive Mode" } else { "Flight Mode" });
        let fill = if response.hovered() && !busy {
            style::tint(0.10)
        } else {
            style::tint(0.06)
        };
        let painter = ui.painter();
        let alpha = if busy { 0.45 } else { 1.0 };
        let stroke = if self.mode_open {
            Stroke::new(1.0, style::ACCENT)
        } else {
            Stroke::NONE
        };
        if self.mode_open {
            painter.rect_stroke(
                rect.expand(2.0),
                style::RADIUS_BTN + 2,
                Stroke::new(2.0, style::rgba(0x1B1F26, 0.10)),
                egui::StrokeKind::Inside,
            );
        }
        painter.rect(
            rect,
            style::RADIUS_BTN,
            fill.gamma_multiply(alpha),
            stroke,
            egui::StrokeKind::Inside,
        );
        let color = if placeholder { style::TEXT_3 } else { style::TEXT_1 };
        painter.galley(
            Pos2::new(rect.left() + 11.0, rect.center().y - text.size().y / 2.0),
            text,
            color.gamma_multiply(alpha),
        );
        let chevron = Rect::from_center_size(Pos2::new(rect.right() - 8.0 - 6.0, rect.center().y), Vec2::splat(12.0));
        icons::paint(ui, "chevron-down", chevron, style::TEXT_3);
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
        let row_h = 7.0 * 2.0 + 12.0 * 1.21;
        let width = rect.width().max(160.0);
        let menu = Rect::from_min_size(
            rect.left_bottom() + Vec2::new(0.0, 4.0),
            Vec2::new(width, 10.0 + row_h * rows.len() as f32),
        );
        let mut chosen = None;
        egui::Area::new(ui.scope_id().with("modemenu"))
            .order(egui::Order::Foreground)
            .fixed_pos(menu.min)
            .show(ctx, |ui| {
                let painter = ui.painter().clone();
                style::glass_panel(&painter, "modemenu", menu, style::RADIUS, 0.6);
                ui.allocate_rect(menu, egui::Sense::hover());
                for (i, (m, enabled)) in rows.iter().enumerate() {
                    let r = Rect::from_min_size(
                        menu.min + Vec2::new(5.0, 5.0 + i as f32 * row_h),
                        Vec2::new(width - 10.0, row_h),
                    );
                    let resp = ui.interact(
                        r,
                        ui.scope_id().with(("mode", i)),
                        if *enabled {
                            egui::Sense::click()
                        } else {
                            egui::Sense::hover()
                        },
                    );
                    let active = m.value == current;
                    if active {
                        painter.rect_filled(r, style::RADIUS_BTN, style::SURFACE_HOVER);
                    } else if resp.hovered() && *enabled {
                        painter.rect_filled(r, style::RADIUS_BTN, style::tint(0.06));
                    }
                    let color = if !enabled {
                        style::TEXT_3
                    } else if active || resp.hovered() {
                        style::TEXT_1
                    } else {
                        style::TEXT_2
                    };
                    let g = style::galley(ui, &m.label, 12.0, Weight::W400, color, 0.0);
                    painter.galley(Pos2::new(r.left() + 8.0, r.center().y - g.size().y / 2.0), g, color);
                    if resp.clicked() && *enabled {
                        chosen = Some(m.value.clone());
                    }
                }
            });
        let pressed_elsewhere = ui.input(|i| i.pointer.any_pressed())
            && !ui
                .input(|i| i.pointer.interact_pos())
                .is_some_and(|p| menu.contains(p) || rect.contains(p));
        if pressed_elsewhere || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.mode_open = false;
        }
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
        let title = style::galley(ui, "TAKEOFF ALTITUDE", 11.0, Weight::W600, style::TEXT_2, 1.0);
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
                    style::TEXT_2,
                );
                // .icon-btn with the x
                let close =
                    Rect::from_center_size(Pos2::new(inner.right() - 10.0, inner.top() + 10.0), Vec2::splat(20.0));
                let resp = ui.interact(close, ui.scope_id().with("takeoffclose"), egui::Sense::click());
                if resp.hovered() {
                    painter.rect_filled(close, 5, style::SURFACE_HOVER);
                }
                icons::paint(
                    ui,
                    "x",
                    close.shrink(3.0),
                    if resp.hovered() { style::TEXT_1 } else { style::TEXT_3 },
                );
                if resp.clicked() {
                    self.takeoff_open = false;
                }

                // The slider row: the track takes what the entry leaves.
                let row_y = inner.top() + 20.0 + 12.0 + 15.0;
                let unit_w = 18.0f32.max(
                    style::galley(ui, units.length.symbol, 12.0, Weight::W600, style::TEXT_2, 0.0)
                        .size()
                        .x,
                );
                let entry_w = 64.0;
                let track = egui::Rangef::new(inner.left(), inner.right() - 10.0 - entry_w - 4.0 - unit_w);
                let pos = altitude_to_position(self.takeoff_alt, TAKEOFF_MIN_M, TAKEOFF_MAX_M, TAKEOFF_SPAN);
                let thumb_x = track.min + (track.max - track.min) * (pos / TAKEOFF_SPAN) as f32;
                let track_rect = Rect::from_x_y_ranges(track, egui::Rangef::new(row_y - 2.0, row_y + 2.0));
                painter.rect_filled(track_rect, 2, style::SURFACE_3);
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
                    style::ACCENT,
                    Stroke::new(2.0, style::TEXT_1),
                );

                // The entry: 64 px, 15 px semibold green, right aligned.
                let entry = Rect::from_min_size(Pos2::new(track.max + 10.0, row_y - 15.0), Vec2::new(entry_w, 30.0));
                let mut child = ui.new_child(egui::UiBuilder::new().max_rect(entry));
                let edit = egui::TextEdit::singleline(&mut self.takeoff_text)
                    .font(style::font(15.0, Weight::W600))
                    .text_color(style::HEALTHY)
                    .align(egui::Align2::RIGHT_CENTER)
                    .margin(egui::Margin::symmetric(6, 5))
                    .desired_width(entry_w - 12.0);
                let typed = child.add(edit);
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
                let unit = style::galley(ui, units.length.symbol, 12.0, Weight::W600, style::TEXT_2, 0.0);
                painter.galley(
                    Pos2::new(entry.right() + 4.0, row_y - unit.size().y / 2.0 + 1.0),
                    unit,
                    style::TEXT_2,
                );

                // .takeoff-confirm
                let button = Rect::from_min_size(
                    Pos2::new(inner.left(), inner.bottom() - confirm_h),
                    Vec2::new(inner.width(), confirm_h),
                );
                let busy = self.busy(Command::Takeoff);
                let resp = ui.interact(button, ui.scope_id().with("takeoffconfirm"), egui::Sense::click());
                let fill = if resp.hovered() {
                    style::HEALTHY_BRIGHT
                } else {
                    style::HEALTHY
                };
                let alpha = if busy { 0.45 } else { 1.0 };
                painter.rect_filled(button, style::RADIUS_BTN, fill.gamma_multiply(alpha));
                let label = style::galley(ui, "CONFIRM TAKEOFF", 12.0, Weight::W700, style::TEXT_INVERTED, 0.6);
                let total = 15.0 + 6.0 + label.size().x;
                let start = button.center().x - total / 2.0;
                icons::paint(
                    ui,
                    "arrow-up",
                    Rect::from_min_size(Pos2::new(start, button.center().y - 7.5), Vec2::splat(15.0)),
                    style::TEXT_INVERTED,
                );
                painter.galley(
                    Pos2::new(start + 21.0, button.center().y - label.size().y / 2.0),
                    label,
                    style::TEXT_INVERTED,
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

/// One `.fa-btn`: icon over caption; hover lifts it, `.armed` fills it
/// green, disabled is 40 % and busy 50 % with a pulsing ring.
#[allow(clippy::too_many_arguments)]
fn fa_button(
    ui: &mut egui::Ui,
    r: Rect,
    icon: &'static str,
    caption: &str,
    title: &str,
    enabled: bool,
    busy: bool,
    armed: bool,
) -> bool {
    let id = ui.scope_id().with(("fa", caption));
    let sense = if enabled {
        egui::Sense::click()
    } else {
        egui::Sense::hover()
    };
    let resp = ui.interact(r, id, sense).on_hover_text(title);
    let hovered = resp.hovered() && enabled && !busy;
    let (fill, color) = if armed {
        (
            if hovered { style::HEALTHY_BRIGHT } else { style::HEALTHY },
            style::TEXT_INVERTED,
        )
    } else if hovered {
        (style::SURFACE_HOVER, style::TEXT_1)
    } else {
        (Color32::TRANSPARENT, style::TEXT_2)
    };
    let alpha = if !enabled {
        0.4
    } else if busy {
        0.5
    } else {
        1.0
    };
    let painter = ui.painter();
    painter.rect_filled(r, style::RADIUS_BTN, fill.gamma_multiply(alpha));
    let caption_g = style::galley(ui, caption, 10.0, Weight::W600, color.gamma_multiply(alpha), 0.6);
    let total = 17.0 + 3.0 + caption_g.size().y;
    let top = r.center().y - total / 2.0;
    icons::paint(
        ui,
        icon,
        Rect::from_center_size(Pos2::new(r.center().x, top + 8.5), Vec2::splat(17.0)),
        color.gamma_multiply(alpha),
    );
    painter.galley(
        Pos2::new(r.center().x - caption_g.size().x / 2.0, top + 20.0),
        caption_g,
        color.gamma_multiply(alpha),
    );
    if busy {
        let t = ui.input(|i| i.time) as f32;
        let pulse = 0.675 + 0.325 * (t / 1.6 * std::f32::consts::TAU).cos();
        painter.rect_stroke(
            r,
            style::RADIUS_BTN,
            Stroke::new(2.0, style::ACCENT_BRIGHT.gamma_multiply(pulse)),
            egui::StrokeKind::Outside,
        );
        ui.ctx().request_repaint();
    }
    resp.clicked()
}

#[cfg(test)]
mod tests {
    use super::*;

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
