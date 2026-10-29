//! The Setup page (src/js/setup.js, setup.css): the head with the vehicle
//! card, the grid of twelve tiles, and the sub-pages behind them.
//!
//! Parameters, Safety & Sensors, Motors, Battery & Power and Calibration are whole. The
//! other tiles open a notice that the page is still in the web interface.

pub mod battery;
pub mod battery_model;
pub mod calib_figure;
pub mod calib_protocol;
pub mod calibration;
pub mod check;
pub mod checklist;
pub mod geofence;
pub mod geofence_card;
pub mod landing;
pub mod motors;
pub mod params;
pub mod params_dialogs;
pub mod params_files;
pub mod safety;
pub mod schema_form;
pub mod sensor_view;
pub mod sensors;

use eframe::egui::{self, Pos2, Rect, Stroke, Vec2};
use serde_json::Value;

use crate::net::http::Server;
use crate::setup::battery::BatteryPage;
use crate::setup::calibration::CalibrationPage;
use crate::setup::motors::MotorsPage;
use crate::setup::params::ParamsPage;
use crate::setup::safety::SafetyPage;
use crate::ui::card;
use crate::ui::pagekit;
use crate::ui::style::{self, Weight};

const TILES: &[(&str, &str, &str, &str)] = &[
    (
        "firstsetup",
        "wand-sparkles",
        "First Setup",
        "A new controller, step by step: firmware, aircraft, calibration.",
    ),
    (
        "calibration",
        "sliders-horizontal",
        "Calibration",
        "Accelerometer, compass, gyro, level, ESCs.",
    ),
    (
        "control",
        "radio",
        "Radio Control",
        "Transmitter, channels, flight modes and switches.",
    ),
    (
        "tuning",
        "activity",
        "PID Tuning",
        "Controller gains and in-flight autotune.",
    ),
    ("motors", "fan", "Motors", "Frame geometry, motor order and protocol."),
    (
        "safety",
        "shield",
        "Safety & Sensors",
        "Limits, failsafes, sensors, preflight checklist.",
    ),
    (
        "battery",
        "battery-charging",
        "Battery & Power",
        "Cells, capacity and power module.",
    ),
    (
        "sik",
        "radio-tower",
        "Telemetry Radio",
        "SiK radio pair: network ID, rate, power.",
    ),
    (
        "rtk",
        "satellite-dish",
        "RTK GPS",
        "Corrections from a base or NTRIP caster.",
    ),
    (
        "remoteid",
        "id-card",
        "Remote ID",
        "Serial number, operator ID and EU class.",
    ),
    (
        "parameters",
        "list",
        "Parameters",
        "Edit, load and save every parameter.",
    ),
    ("firmware", "cpu", "Firmware", "Flash PX4 or ArduPilot over direct USB."),
    ("video", "video", "Video", "RTSP or WebRTC cameras in floating windows."),
];

/// Whether `id` names a sub page (`--page`): a tile, or `safety:SENSOR`.
pub fn is_page(id: &str) -> bool {
    TILES.iter().any(|t| t.0 == id) || id.starts_with("safety:")
}

/// Flight stack and version in one value, e.g. `PX4 v1.18.0`.
pub fn firmware_text(s: &Value) -> String {
    let part = |k: &str| s.get(k).and_then(Value::as_str).filter(|t| !t.is_empty());
    let text: Vec<&str> = [part("autopilot"), part("px4_version")].into_iter().flatten().collect();
    if text.is_empty() {
        "\u{2014}".into()
    } else {
        text.join(" ")
    }
}

/// How many tile columns `auto-fill, minmax(260px, 1fr)` makes in `w`.
pub fn columns(w: f32) -> usize {
    (((w + 14.0) / (260.0 + 14.0)).floor() as usize).max(1)
}

enum View {
    Tiles,
    Parameters(Box<ParamsPage>),
    Safety(Box<SafetyPage>),
    Motors(Box<MotorsPage>),
    Battery(Box<BatteryPage>),
    Calibration(Box<CalibrationPage>),
    Elsewhere(&'static str),
}

/// The page's state.
pub struct SetupPage {
    server: Server,
    env: crate::setup::geofence::Env,
    /// The display units, set by the app each frame.
    pub units: crate::data::units::Units,
    view: View,
    /// A scroll offset to open at (the --scroll flag, for looking at a page).
    initial_scroll: Option<f32>,
}

impl SetupPage {
    pub fn new(server: Server, env: crate::setup::geofence::Env) -> SetupPage {
        SetupPage {
            server,
            env,
            units: crate::data::units::Units::default(),
            view: View::Tiles,
            initial_scroll: None,
        }
    }

    /// Errors for the notification board.
    pub fn take_errors(&mut self) -> Vec<String> {
        match &mut self.view {
            View::Parameters(p) => p.take_errors(),
            View::Safety(p) => p.take_errors(),
            View::Motors(p) => p.take_errors(),
            View::Battery(p) => p.take_errors(),
            View::Calibration(p) => p.take_errors(),
            _ => Vec::new(),
        }
    }

    /// Notices for the notification board.
    pub fn take_notes(&mut self) -> Vec<String> {
        match &mut self.view {
            View::Parameters(p) => p.take_notes(),
            View::Calibration(p) => p.take_notes(),
            _ => Vec::new(),
        }
    }

    /// Warnings for the notification board.
    pub fn take_warnings(&mut self) -> Vec<String> {
        match &mut self.view {
            View::Calibration(p) => p.take_warnings(),
            _ => Vec::new(),
        }
    }

    /// Opens the page scrolled by `px`.
    pub fn scroll_to(&mut self, px: f32) {
        self.initial_scroll = Some(px);
    }

    /// Opens a tile's page, as clicking it does.
    pub fn open(&mut self, id: &str, ctx: &egui::Context) {
        let (id, sensor) = match id.split_once(':') {
            Some((page, sensor)) => (page, Some(sensor)),
            None => (id, None),
        };
        self.view = match id {
            "parameters" => View::Parameters(Box::new(ParamsPage::new(self.server.clone(), ctx))),
            "safety" => {
                let mut page = SafetyPage::new(self.server.clone(), ctx, self.env.clone());
                if let Some(sensor) = sensor {
                    page.open_sensor(sensor);
                }
                View::Safety(Box::new(page))
            }
            "motors" => View::Motors(Box::new(MotorsPage::new(self.server.clone(), ctx))),
            "battery" => View::Battery(Box::new(BatteryPage::new(self.server.clone(), ctx))),
            "calibration" => View::Calibration(Box::new(CalibrationPage::new(self.server.clone(), ctx))),
            other => match TILES.iter().find(|t| t.0 == other) {
                Some(t) => View::Elsewhere(t.2),
                None => View::Tiles,
            },
        };
    }

    /// Back to the tiles (leaving the left rail's SETUP does the same).
    pub fn reset(&mut self) {
        self.view = View::Tiles;
    }

    /// Draws the page into `rect` (the page view).
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        rect: Rect,
        state: &Value,
        now: f64,
        checklist: &mut crate::setup::checklist::Checklist,
    ) {
        ui.painter().rect_filled(rect, 0, style::bg());
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
        child.set_clip_rect(rect);
        let mut area = egui::ScrollArea::vertical()
            .id_salt("setupscroll")
            .auto_shrink([false, false]);
        if let Some(px) = self.initial_scroll.take() {
            area = area.vertical_scroll_offset(px);
        }
        area.show(&mut child, |ui| {
            let x = rect.left() + pagekit::PAD_X;
            let w = rect.width() - 2.0 * pagekit::PAD_X;
            let top = ui.max_rect().top();
            let mut y = top + pagekit::PAD_Y;
            match &mut self.view {
                View::Tiles => {
                    y = head(
                        ui,
                        x,
                        y,
                        w,
                        "Setup",
                        "Vehicle configuration and calibration",
                        Some(state),
                        |_, _, _| 0.0,
                    );
                    y = grid(ui, x, y, w, &mut self.view, &self.server, &self.env);
                }
                View::Parameters(_)
                | View::Safety(_)
                | View::Motors(_)
                | View::Battery(_)
                | View::Calibration(_)
                | View::Elsewhere(_) => {
                    let in_sensor = matches!(&self.view, View::Safety(p) if p.in_sensor());
                    let in_wizard = matches!(&self.view, View::Calibration(p) if p.in_wizard());
                    let (bh, back) = pagekit::back_button(
                        ui,
                        Pos2::new(x, y),
                        if in_sensor {
                            "Safety & Sensors"
                        } else if in_wizard {
                            "Calibration"
                        } else {
                            "Setup"
                        },
                    );
                    y += bh + 22.0;
                    if back && in_sensor {
                        if let View::Safety(p) = &mut self.view {
                            p.close_sensor();
                        }
                    } else if back && in_wizard {
                        if let View::Calibration(p) = &mut self.view {
                            p.close_wizard();
                        }
                    } else if back {
                        self.view = View::Tiles;
                    } else if let View::Motors(p) = &mut self.view {
                        y = p.show(ui, x, y, w, state, now);
                    } else if let View::Battery(p) = &mut self.view {
                        y = p.show(ui, x, y, w, state, &self.units);
                    } else if let View::Calibration(p) = &mut self.view {
                        y = p.show(ui, x, y, w, state, now);
                    } else if let View::Safety(p) = &mut self.view {
                        y = p.show(ui, x, y, w, state, checklist);
                    } else if let View::Parameters(p) = &mut self.view {
                        y = p.show(ui, x, y, w, state, now);
                    } else if let View::Elsewhere(name) = &self.view {
                        y = head(ui, x, y, w, name, "", None, |_, _, _| 0.0);
                        let g = crate::ui::kit::wrapped_weight(
                            ui,
                            "This page is not in the native interface yet. It is still in the web interface.",
                            12.0,
                            Weight::W400,
                            style::text_2(),
                            w - 28.0,
                            1.55,
                        );
                        let r = Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, g.size().y + 24.0));
                        {
                            ui.painter().rect_filled(r, style::RADIUS_BTN, style::surface_2());
                            ui.painter().rect_stroke(
                                r,
                                style::RADIUS_BTN,
                                Stroke::new(1.0, style::border_soft()),
                                egui::StrokeKind::Inside,
                            );
                        }
                        ui.painter().galley(r.min + Vec2::new(14.0, 12.0), g, style::text_2());
                        y = r.bottom();
                    }
                }
            }
            ui.allocate_space(Vec2::new(rect.width() - 1.0, y + pagekit::PAD_Y - top));
        });
    }
}

/// `.setup-head`: the page header, and `side` or the vehicle card at the
/// right on the subtitle's line; returns the y under it.
#[allow(clippy::too_many_arguments)]
fn head(
    ui: &mut egui::Ui,
    x: f32,
    y: f32,
    w: f32,
    title: &str,
    subtitle: &str,
    vehicle: Option<&Value>,
    side: impl FnOnce(&mut egui::Ui, f32, f32) -> f32,
) -> f32 {
    let result = if let Some(s) = vehicle {
        let facts = [
            (
                "Vehicle Type",
                s.get("vehicle_type")
                    .and_then(Value::as_str)
                    .filter(|t| !t.is_empty())
                    .unwrap_or("\u{2014}")
                    .to_string(),
            ),
            ("Firmware", firmware_text(s)),
        ];
        let gal: Vec<_> = facts
            .iter()
            .map(|(l, v)| {
                (
                    style::galley(ui, &l.to_uppercase(), 11.0, Weight::W600, style::text_3(), 0.6),
                    style::galley_mono(ui, v, 13.0, style::text_1()),
                )
            })
            .collect();
        let widths: Vec<f32> = gal.iter().map(|(l, v)| 32.0 + l.size().x.max(v.size().x)).collect();
        let card_w: f32 = widths.iter().sum::<f32>() + 2.0;
        let card_h = 14.0 + 2.0 + gal[0].0.size().y + 2.0 + gal[0].1.size().y;
        // the card wraps under the title when it would leave it under 280 px
        let wrap = w - card_w - 24.0 < 280.0;
        let head_bottom = pagekit::header_wrapped(
            ui,
            x,
            y,
            if wrap { w } else { (w - card_w - 24.0).min(w) },
            title,
            subtitle,
        );
        let (card_x, bottom) = if wrap {
            (x, head_bottom + 10.0 + card_h)
        } else {
            (x + w - card_w, head_bottom)
        };
        let card = Rect::from_min_size(Pos2::new(card_x, bottom - card_h), Vec2::new(card_w, card_h));
        let p = ui.painter();
        card::paint(p, card, card::Look::Page);
        let mut fx = card.left() + 1.0;
        for (i, (l, v)) in gal.into_iter().enumerate() {
            if i > 0 {
                p.vline(fx, card.y_range().shrink(8.0), Stroke::new(1.0, style::border_soft()));
            }
            p.galley(Pos2::new(fx + 16.0, card.top() + 7.0), l.clone(), style::text_3());
            p.galley(
                Pos2::new(fx + 16.0, card.top() + 7.0 + l.size().y + 2.0),
                v,
                style::text_1(),
            );
            fx += widths[i];
        }
        bottom
    } else {
        let bottom = pagekit::header(ui, x, y, title, subtitle);
        let _ = side(ui, x + w, bottom);
        bottom
    };
    result + 26.0
}

/// The tile grid.
fn grid(
    ui: &mut egui::Ui,
    x: f32,
    y: f32,
    w: f32,
    view: &mut View,
    server: &Server,
    env: &crate::setup::geofence::Env,
) -> f32 {
    let cols = columns(w);
    let tile_w = (w - 14.0 * (cols as f32 - 1.0)) / cols as f32;
    let tile_h = 18.0 * 2.0 + (14.0 * 1.21f32 + 3.0 + 2.0 * 11.5 * 1.45).max(40.0);
    let mut bottom = y;
    for (i, (id, icon, title, desc)) in TILES.iter().enumerate() {
        let (col, row) = ((i % cols) as f32, (i / cols) as f32);
        let r = Rect::from_min_size(
            Pos2::new(x + col * (tile_w + 14.0), y + row * (tile_h + 14.0)),
            Vec2::new(tile_w, tile_h),
        );
        bottom = bottom.max(r.bottom());
        if pagekit::tile(ui, r, icon, title, desc) {
            *view = match *id {
                "parameters" => View::Parameters(Box::new(ParamsPage::new(server.clone(), ui.ctx()))),
                "safety" => View::Safety(Box::new(SafetyPage::new(server.clone(), ui.ctx(), env.clone()))),
                "motors" => View::Motors(Box::new(MotorsPage::new(server.clone(), ui.ctx()))),
                "battery" => View::Battery(Box::new(BatteryPage::new(server.clone(), ui.ctx()))),
                "calibration" => View::Calibration(Box::new(CalibrationPage::new(server.clone(), ui.ctx()))),
                _ => View::Elsewhere(title),
            };
        }
    }
    bottom
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_firmware_line() {
        assert_eq!(
            firmware_text(&json!({"autopilot": "PX4", "px4_version": "v1.18.0"})),
            "PX4 v1.18.0"
        );
        assert_eq!(firmware_text(&json!({"autopilot": "ArduPilot"})), "ArduPilot");
        assert_eq!(firmware_text(&json!({})), "\u{2014}");
    }

    #[test]
    fn the_grid_fills_as_css_auto_fill_does() {
        assert_eq!(columns(100.0), 1);
        assert_eq!(columns(260.0), 1);
        assert_eq!(columns(534.0), 2);
        assert_eq!(columns(808.0), 3);
        assert_eq!(TILES.len(), 13);
    }
}
