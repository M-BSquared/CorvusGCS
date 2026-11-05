//! The pills whose cards are switches and pickers (`renderInterfaceTab`,
//! `renderLayoutTab`, `renderFlightTab`, `renderControlsTab` and the cards
//! under them in `python/src/js/sidenav.js`).

use eframe::egui::{self, Pos2, Rect, Vec2};
use serde_json::{json, Value};

use super::model;
use super::SettingsPage;
use crate::ui::form::{self, Form, CONTROL_H};
use crate::ui::kit::{self, Button, Choice, Variant};
use crate::ui::message::Tone;
use crate::ui::optioncards::Card;

/// One switch of the config.
pub(super) struct Sw {
    pub(super) section: &'static str,
    pub(super) key: &'static str,
    pub(super) default: bool,
    pub(super) label: &'static str,
    pub(super) hint: &'static str,
}

const fn ui_sw(key: &'static str, default: bool, label: &'static str, hint: &'static str) -> Sw {
    Sw {
        section: "ui",
        key,
        default,
        label,
        hint,
    }
}

const MISSION: Sw = ui_sw(
    "mission_page",
    false,
    "Mission planner",
    "Adds MISSION under HOME in the left rail: a planning map with takeoff, waypoints, orbits and landing, and an altitude profile of the whole flight you can drag each point's height on. Missions are drawn, saved and uploaded there. Nothing is sent to the aircraft until you press Upload.",
);

const SIDE_PANEL: [Sw; 2] = [
    ui_sw(
        "console_tab",
        true,
        "MAVLink console tab",
        "The CONSOLE tab: the autopilot's messages and a command line. Off, the tab is gone. Messages are still collected, and Logs still exports them.",
    ),
    ui_sw(
        "ssh_tab",
        true,
        "SSH tab",
        "The SSH tab: your saved connections and a terminal. Off, the tab is gone. Connections stay saved, plugins still use them, and a terminal opens in a window of its own.",
    ),
];

const NOTIFICATIONS: Sw = ui_sw(
    "notification_marks",
    false,
    "Severity marks",
    "Coloured bars above and below the level icon on a notification: blue for info, amber for a warning, red for a fault. Off by default. The icon already carries the level in its colour. Turn them on to underline it.",
);

const TOP_BAR_DOTS: Sw = ui_sw(
    "topbar_status_dots",
    false,
    "Status dots",
    "Adds a state dot after the VEHICLE, STATUS, GPS and BATTERY captions: green good, amber caution, red fault, grey nothing to report. Off by default: the values already carry that colour themselves. Turn it on to scan the bar by colour alone.",
);

const TOP_BAR_BLOCKS: [Sw; 4] = [
    ui_sw(
        "topbar_show_altitude",
        true,
        "Show altitude",
        "Shows the ALTITUDE block in the top bar. Hover over it for the height above sea level, above home and the GPS altitude. On by default.",
    ),
    ui_sw(
        "topbar_show_rangefinder",
        false,
        "Show distance sensor",
        "Adds the reading of a downward distance sensor to the altitude card. Off by default: while it is off, Corvus does not ask the vehicle for it.",
    ),
    ui_sw(
        "topbar_show_groundspeed",
        true,
        "Show groundspeed",
        "Shows the GROUNDSPEED block in the top bar. On by default.",
    ),
    ui_sw(
        "topbar_show_vspeed",
        true,
        "Show climb rate",
        "Shows the VERTICAL SPEED block, the climb or sink rate, in the top bar. On by default.",
    ),
];

const FLIGHT_BAR: [Sw; 2] = [
    ui_sw(
        "flight_bar_shrink",
        false,
        "Shrink with the window",
        "Off, the bar keeps its full size and only narrows when the row will not fit, the same rule the Mission planner's tools are under. On, ARM, TAKEOFF, LAND, RETURN and PLAN start narrowing as soon as the row would cover more than half the map: the buttons drop to the width of their own word first, then to icons alone. Trade the words for map only if you want to.",
    ),
    ui_sw(
        "takeoff_land_split",
        false,
        "Separate Takeoff and Land buttons",
        "Off, the bar has one button: TAKEOFF while the aircraft is on the ground, LAND once it is flying. The takeoff card opens only from TAKEOFF, never from LAND. On, TAKEOFF and LAND are two buttons side by side, and the one that does not apply is greyed.",
    ),
];

const HUD: [Sw; 2] = [
    ui_sw(
        "flight_hud",
        true,
        "Show the flight HUD",
        "On (the default), the window with the compass, the attitude indicator and the readouts sits over the Home map. Off, it is gone until you turn it back on, and it comes back where you left it.",
    ),
    ui_sw(
        "hud_square",
        false,
        "Square HUD corners",
        "Off (the default), the window has a rounded top. On, all four corners are square. The bar with the pin and size buttons stays where it is.",
    ),
];

const COMPASS: Sw = ui_sw(
    "compass_nose_up",
    false,
    "Lock compass nose up",
    "On, the compass rose turns and the needle always points to the top, so the top of the dial is where the aircraft is heading. Off (the default), north stays up and the needle turns inside the rose, the same way the map is drawn.",
);

pub(super) const TRACK: [Sw; 4] = [
    ui_sw(
        "track_earlier_flights",
        true,
        "Earlier flights in their own colour",
        "On (the default): when the aircraft lands and takes off again, the track of the flights before turns a lighter red, so the flight in progress stands out. Off: every flight is drawn in the same red.",
    ),
    ui_sw(
        "track_rainbow",
        false,
        "Rainbow colours for earlier flights",
        "Off (the default): every earlier flight has the same lighter red. On: each earlier flight gets a colour of its own, still faded, so flights over the same ground can be told apart. The flight in progress keeps the aircraft's colour. Needs earlier flights in their own colour.",
    ),
    ui_sw(
        "track_clear_on_restart",
        false,
        "Clear the track on restart",
        "Off (the default): the track stays on the map when Corvus or the autopilot restarts, until you clear it with the button on the map. On: it is cleared every time Corvus starts and every time the autopilot reboots.",
    ),
    ui_sw(
        "track_in_air",
        true,
        "Track in the air in 3D",
        "On (the default): in 3D the track is also drawn as a thin line at the height it was flown, above the track on the ground. Off: only the track on the ground.",
    ),
];

pub(super) const MEASURE: Sw = ui_sw(
    "measure_tool",
    true,
    "Show the distance tool",
    "On (the default), the ruler button sits in the map's right-hand bar. Press it, click a start and an end point, and the map shows the distance and heading between them. Escape or a right click clears the line, and a second one turns the tool off. Off, the button is gone.",
);

const APP_ICON: [Sw; 2] = [
    ui_sw(
        "inverted_app_icon",
        false,
        "Inverted app icon",
        "Uses the black mark instead of the white one for the Dock (macOS) or taskbar (Linux) icon, better on a light dock. Applies within a second, and changes nothing else.",
    ),
    ui_sw(
        "app_icon_backplate",
        false,
        "Icon backplate",
        "Draws the mark on a filled rounded square (dark behind the white mark, white behind the black one) so it stays visible against a dock of any shade.",
    ),
];

const APP_ICON_SCOPE: &str = "Finder and the Start menu keep the icon the build shipped, because it lives inside the signed bundle. On Linux the applications grid and the .AppImage\u{2019}s own thumbnail follow.";

const CONTROLS: [(&str, &str, &str, &str); 4] = [
    (
        "virtual_joystick",
        "settingsVirtualJoystick",
        "Virtual joystick",
        "Two-stick pad over the map: left is throttle and yaw, right is pitch and roll. Drag it by the grip bar, double-click the bar to send it back.",
    ),
    (
        "gamepad",
        "settingsGamepad",
        "USB gamepad",
        "Flies from a game controller plugged in by USB, with the same layout as the virtual joystick. To arm, push both sticks inward, toward each other, and hold them there for one second. After arming, the sticks do nothing until both are let go.",
    ),
    (
        "arrow_keys",
        "settingsArrowKeys",
        "Arrow keys",
        "Flies pitch and roll from the arrow keys (on screen and on your own keyboard) at the strength set below.",
    ),
    (
        "wasd_keys",
        "settingsWasdKeys",
        "WASD keys",
        "The other half: W/S are thrust, A/D are yaw, at the same strength; a released thrust key returns to the hover detent. Any of these controls reach the aircraft only with COM_RC_IN_MODE 1, 2 or 3 and a mode that flies from the sticks. Off by default.",
    ),
];

const KEY_STRENGTH_HINT: &str = "How far a held arrow or W/A/S/D key pushes the stick, as a share of full travel. Hold Shift for twice that while it is down. Keys only. The sticks always reach their own stops. 50% is the default.";

const TOUCH_HINT: &str = "Larger buttons for a finger, a long press where a mouse would right click, and none of the parts that need a mouse or a keyboard, such as the SSH terminal tab. Saved on this device only, so a tablet and the computer it connects to each keep their own.";

const LANGUAGE_INFO: &str = "More languages can be added: copy a language file into the languages folder of your Corvus folder (~/.corvus/languages), then press Reload languages. One file per language, named after its code, for example es.json. A text a file does not list stays English.\nA file there with the code of a language that ships with Corvus replaces it.";

impl SettingsPage {
    fn switches(&mut self, ui: &mut egui::Ui, f: &mut Form, rows: &[Sw]) {
        for r in rows {
            let on = model::flag(&self.cfg, r.section, r.key, r.default);
            if let Some(v) = form::switch(ui, f, ("sw", r.key), r.label, r.hint, on) {
                self.write(ui.ctx(), model::patch(r.section, r.key, json!(v)), true);
            }
        }
    }

    /// A select in a `.field`, returning the value picked.
    pub(super) fn select_field(
        ui: &mut egui::Ui,
        f: &mut Form,
        label: &str,
        hint: &str,
        choices: &[Choice],
        selected: &str,
        id: &str,
    ) -> Option<String> {
        let mut picked = None;
        form::field(ui, f, label, "", hint, |ui, x, y, w| {
            let r = Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, CONTROL_H));
            picked = kit::select(ui, r, choices, selected, "", false, "", id);
            CONTROL_H
        });
        picked
    }

    pub(super) fn interface_tab(&mut self, ui: &mut egui::Ui, f: &mut Form) {
        form::section(ui, f, "General", |ui, f| {
            self.language_card(ui, f);
            self.theme_card(ui, f);
            self.scale_card(ui, f);
            self.units_card(ui, f);
            self.touch_card(ui, f);
        });
        form::section(ui, f, "Branding", |ui, f| {
            self.logo_card(ui, f);
            form::card(ui, f, "App icon", |ui, f| {
                self.switches(ui, f, &APP_ICON);
                f.skip(10.0);
                form::note(ui, f, APP_ICON_SCOPE);
            });
        });
    }

    pub(super) fn layout_tab(&mut self, ui: &mut egui::Ui, f: &mut Form) {
        form::section(ui, f, "Navigation", |ui, f| {
            form::card(ui, f, "Pages", |ui, f| self.switches(ui, f, &[MISSION]));
            form::card(ui, f, "Side panel", |ui, f| self.switches(ui, f, &SIDE_PANEL));
        });
        form::section(ui, f, "Windows and notifications", |ui, f| {
            self.windows_card(ui, f);
            form::card(ui, f, "Notifications", |ui, f| self.switches(ui, f, &[NOTIFICATIONS]));
        });
    }

    pub(super) fn flight_tab(&mut self, ui: &mut egui::Ui, f: &mut Form) {
        form::section(ui, f, "Bars", |ui, f| {
            form::card(ui, f, "Top bar", |ui, f| {
                self.switches(ui, f, &[TOP_BAR_DOTS]);
                let current = model::altitude_ref(&self.cfg);
                let opts: Vec<Card> = model::ALTITUDE_REFS.iter().map(|o| Card::new(o.0, o.1, o.2)).collect();
                let mut picked = None;
                form::field(
                    ui,
                    f,
                    "Altitude",
                    "",
                    "Which altitude the ALTITUDE block shows. AMSL (the default) is height above mean sea level. Relative is height above the home position, usually where the aircraft took off.",
                    |ui, x, y, w| {
                        let mut inner = Form::new(x, y, w);
                        picked = form::option_cards(ui, &mut inner, "altref", &opts, current, 2);
                        inner.y - y
                    },
                );
                if let Some(id) = picked {
                    self.write(ui.ctx(), model::patch("ui", "topbar_altitude", json!(id)), true);
                }
                self.switches(ui, f, &TOP_BAR_BLOCKS);
            });
            form::card(ui, f, "Flight bar", |ui, f| self.switches(ui, f, &FLIGHT_BAR));
        });
        form::section(ui, f, "Instruments", |ui, f| {
            form::card(ui, f, "Flight HUD", |ui, f| self.switches(ui, f, &HUD));
            form::card(ui, f, "Compass", |ui, f| self.switches(ui, f, &[COMPASS]));
        });
    }

    pub(super) fn controls_tab(&mut self, ui: &mut egui::Ui, f: &mut Form) {
        form::section(ui, f, "Manual control", |ui, f| {
            form::card(ui, f, "Controls", |ui, f| {
                for (key, id, label, hint) in CONTROLS {
                    let on = model::flag(&self.cfg, "controls", key, false);
                    if let Some(v) = form::switch(ui, f, id, label, hint, on) {
                        self.write(ui.ctx(), model::patch("controls", key, json!(v)), true);
                    }
                }
                form::ruled(ui, f);
                let gain = model::get(&self.cfg, "controls", "key_gain")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.5) as f32;
                let cur = model::nearest_step(&model::GAIN_STEPS, gain);
                let steps: Vec<String> = model::GAIN_STEPS.iter().map(|s| s.1.to_string()).collect();
                let mut moved = None;
                form::field(
                    ui,
                    f,
                    "Key strength: arrow keys and WASD",
                    "",
                    KEY_STRENGTH_HINT,
                    |ui, x, y, w| {
                        let mut inner = Form::new(x, y, w);
                        moved = form::slider(ui, &mut inner, "keygain", &steps, cur);
                        inner.y - y
                    },
                );
                if let Some((i, released)) = moved {
                    let v = json!(model::GAIN_STEPS[i].0);
                    if released {
                        if i != cur {
                            self.write(ui.ctx(), model::patch("controls", "key_gain", v), true);
                        }
                    } else if i != cur {
                        model::merge(&mut self.cfg, &model::patch("controls", "key_gain", v));
                    }
                }
            });
        });
    }

    fn windows_card(&mut self, ui: &mut egui::Ui, f: &mut Form) {
        let frost = if cfg!(target_os = "macos") {
            " and the desktop behind a window of its own."
        } else {
            ". A terminal in a window of its own is solid here either way, because only macOS blurs what is behind a window."
        };
        let solid_hint = format!(
            "Off (the default): terminal windows are frosted glass and blur what is behind them, the map inside the Corvus window{frost} On: they are solid, which reads best in bright light."
        );
        let rows: [(&str, &str, &str); 3] = [
            (
                "windows_in_app",
                "Open inside the Corvus window",
                "Off (the default): a camera or terminal opens as a window of its own straight away, which can go anywhere, a second screen included. On: it opens inside the Corvus window, and becomes a window of its own when you drag it past the edge.",
            ),
            (
                "corvus_title_bar",
                "Corvus title bar",
                "Off (the default): a window of its own has the title bar of the operating system, with its own buttons to close, minimize and maximize it. The name, the status, the pin and the power button stay in the Corvus bar. On: the Corvus bar is the whole title bar, with its own buttons, as inside the app.",
            ),
            ("solid_terminals", "Solid terminals", &solid_hint),
        ];
        form::card(ui, f, "Camera and terminal windows", |ui, f| {
            for (key, label, hint) in rows {
                let on = model::flag(&self.cfg, "ui", key, false);
                if let Some(v) = form::switch(ui, f, ("win", key), label, hint, on) {
                    self.write(ui.ctx(), model::patch("ui", key, json!(v)), true);
                }
            }
        });
    }

    fn language_card(&mut self, ui: &mut egui::Ui, f: &mut Form) {
        let choices: Vec<Choice> = crate::i18n::languages()
            .into_iter()
            .map(|l| Choice::new(l.code, l.label))
            .collect();
        let current = crate::i18n::language();
        let mut picked = None;
        let mut reload = false;
        form::card(ui, f, "Language", |ui, f| {
            form::field(ui, f, "Interface language", LANGUAGE_INFO, "", |ui, x, y, w| {
                let sel = Rect::from_min_size(Pos2::new(x, y), Vec2::new(240.0f32.min(w), CONTROL_H));
                picked = kit::select(ui, sel, &choices, &current, "", false, "", "language");
                let b = Button::new("Reload languages", Variant::Secondary).icon("refresh-cw");
                let bw = b.width(ui);
                let at = Rect::from_min_size(Pos2::new(sel.right() + 8.0, y), Vec2::new(bw, CONTROL_H));
                reload = b.enabled(true).show(ui, at, "reloadlang");
                CONTROL_H
            });
            if let Some((t, tone)) = self.msgs.get("language").cloned() {
                f.skip(8.0);
                form::message(ui, f, &t, tone);
            }
        });
        if let Some(code) = picked {
            crate::i18n::set_language(&code);
            self.write(ui.ctx(), json!({ "language": code }), false);
        }
        if reload {
            let n = crate::i18n::reload();
            let key = if n == 1 {
                "Reloaded. 1 language installed."
            } else {
                "Reloaded. {n} languages installed."
            };
            self.say("language", crate::i18n::tr_fmt(key, &[("n", &n.to_string())]), Tone::Ok);
        }
    }

    fn theme_card(&mut self, ui: &mut egui::Ui, f: &mut Form) {
        let current = crate::ui::style::theme_from_config(&self.cfg);
        let opts: Vec<Card> = model::THEMES
            .iter()
            .map(|t| Card::new(t.0, t.1, t.2).swatch(t.3))
            .collect();
        let mut picked = None;
        form::card(ui, f, "Color theme", |ui, f| {
            picked = form::option_cards(ui, f, "theme", &opts, current, 2);
        });
        if let Some(id) = picked {
            self.write(ui.ctx(), json!({ "theme": { "name": id } }), false);
        }
    }

    fn scale_card(&mut self, ui: &mut egui::Ui, f: &mut Form) {
        let scale = model::scale_from_config(&self.cfg);
        let cur = model::nearest_step(&model::SCALE_STEPS, scale);
        let steps: Vec<String> = model::SCALE_STEPS.iter().map(|s| s.1.to_string()).collect();
        let mut moved = None;
        form::card(ui, f, "Interface size", |ui, f| {
            moved = form::slider(ui, f, "scale", &steps, cur);
            f.skip(FIELD_GAP);
            form::note(
                ui,
                f,
                "Scales the whole interface on this machine: text, icons, bars and panels. Larger reads better on a bright field laptop. 100% is the default.",
            );
        });
        if let Some((i, released)) = moved {
            let patch = model::patch("ui", "scale", json!(model::SCALE_STEPS[i].0));
            if released {
                if i != cur || (model::SCALE_STEPS[i].0 - scale).abs() > f32::EPSILON {
                    let undo = model::merge(&mut self.cfg, &patch);
                    self.sync_effects();
                    self.post(ui.ctx(), patch, undo, false);
                }
            } else if i != cur {
                model::merge(&mut self.cfg, &patch);
                self.sync_effects();
            }
        }
    }

    /// Tablet mode. Unlike everything else on this page it is not in the
    /// backend config: a tablet reaches Corvus over the network and shares the
    /// laptop's config, so the choice is kept on the device that made it.
    fn touch_card(&mut self, ui: &mut egui::Ui, f: &mut Form) {
        form::card(ui, f, "Tablet mode", |ui, f| {
            if let Some(v) = form::switch(
                ui,
                f,
                "touchmode",
                "Touch screen layout",
                TOUCH_HINT,
                crate::app::touch::on(),
            ) {
                self.touch_request = Some(v);
            }
        });
    }

    fn units_card(&mut self, ui: &mut egui::Ui, f: &mut Form) {
        let units = model::units_of(&self.cfg);
        let preset = model::system_of(&units).unwrap_or("");
        let opts: Vec<Card> = model::UNIT_SYSTEMS.iter().map(|s| Card::new(s.0, s.1, s.2)).collect();
        let mut new_units: Option<[String; 4]> = None;
        form::card(ui, f, "Units", |ui, f| {
            let mut picked = None;
            form::field(
                ui,
                f,
                "Unit system",
                "",
                "Sets all four below at once. You can still change any one of them afterwards.",
                |ui, x, y, w| {
                    let mut inner = Form::new(x, y, w);
                    picked = form::option_cards(ui, &mut inner, "unitsys", &opts, preset, 3);
                    inner.y - y
                },
            );
            if let Some(id) = picked {
                if let Some(s) = model::UNIT_SYSTEMS.iter().find(|s| s.0 == id) {
                    new_units = Some(s.3.map(String::from));
                }
            }
            for (i, (q, label, hint)) in model::UNIT_ROWS.iter().enumerate() {
                f.skip(14.0);
                let choices: Vec<Choice> = model::unit_choices(q).iter().map(|c| Choice::new(c.0, c.1)).collect();
                if let Some(v) =
                    SettingsPage::select_field(ui, f, label, hint, &choices, &units[i], &format!("unit{q}"))
                {
                    let mut next = units.clone();
                    next[i] = v;
                    new_units = Some(next);
                }
            }
            f.skip(14.0);
            form::note(
                ui,
                f,
                "Display only. Parameters, mission files and everything sent to the aircraft stay in the units the autopilot uses.",
            );
        });
        if let Some(u) = new_units {
            self.write(ui.ctx(), model::patch("ui", "units", model::units_value(&u)), false);
        }
    }
}

const FIELD_GAP: f32 = crate::ui::formfield::GAP;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_switch_names_a_key_once_with_the_webs_default() {
        let mut keys: Vec<&str> = Vec::new();
        for s in SIDE_PANEL
            .iter()
            .chain(&TOP_BAR_BLOCKS)
            .chain(&FLIGHT_BAR)
            .chain(&HUD)
            .chain(&TRACK)
            .chain(&APP_ICON)
            .chain([&MISSION, &NOTIFICATIONS, &TOP_BAR_DOTS, &COMPASS, &MEASURE])
        {
            assert!(!keys.contains(&s.key), "{}", s.key);
            keys.push(s.key);
            assert!(
                !s.label.contains('\u{2014}') && !s.hint.contains('\u{2014}'),
                "{}",
                s.key
            );
        }
        let by = |k: &str| {
            [&MISSION, &MEASURE, &COMPASS, &TOP_BAR_DOTS]
                .into_iter()
                .chain(&SIDE_PANEL)
                .chain(&HUD)
                .chain(&TRACK)
                .chain(&TOP_BAR_BLOCKS)
                .find(|s| s.key == k)
                .unwrap()
                .default
        };
        assert!(!by("mission_page") && by("measure_tool") && by("console_tab") && by("ssh_tab"));
        assert!(by("flight_hud") && !by("hud_square") && !by("compass_nose_up"));
        assert!(
            by("track_earlier_flights") && !by("track_rainbow") && !by("track_clear_on_restart") && by("track_in_air")
        );
        assert!(by("topbar_show_altitude") && !by("topbar_show_rangefinder") && by("topbar_show_vspeed"));
    }
}
