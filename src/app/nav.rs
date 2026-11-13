//! The left rail (python/src/js/sidenav.js renderLeftNav, `.left-nav`,
//! `.nav-item`): HOME, MISSION when the config turns the planner on (a
//! hairline otherwise), SETUP, ANALYSIS, LOGS, and SET at the bottom.

use crate::ui::tip::HoverTr;
use eframe::egui::{self, Color32, Pos2, Rect, Vec2};

use crate::ui::icons;
use crate::ui::style::{self, Weight};

/// `--left-w`.
pub const WIDTH: f32 = 70.0;
const ITEM_W: f32 = 58.0;
const ITEM_H: f32 = 52.0;

/// A page the rail switches to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Home,
    Mission,
    Setup,
    Analysis,
    Logs,
    Settings,
}

impl Page {
    /// The rail caption and the Lucide icon.
    pub fn item(self) -> (&'static str, &'static str) {
        match self {
            Page::Home => ("HOME", "house"),
            Page::Mission => ("MISSION", "route"),
            Page::Setup => ("SETUP", "sliders-horizontal"),
            Page::Analysis => ("ANALYSIS", "chart-column"),
            Page::Logs => ("LOGS", "file-text"),
            Page::Settings => ("SET", "settings"),
        }
    }

    /// The page's name, as its own header names it.
    pub fn title(self) -> &'static str {
        match self {
            Page::Home => "Home",
            Page::Mission => "Mission",
            Page::Setup => "Setup",
            Page::Analysis => "Analysis",
            Page::Logs => "Logs",
            Page::Settings => "Settings",
        }
    }
}

/// One slot of the rail, top to bottom.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    Item(Page),
    /// The shortcut to the Betaflight page of Setup, only while a Betaflight
    /// aircraft is connected.
    Betaflight,
    Divider,
    Spacer,
}

/// What a click on the rail asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Click {
    Page(Page),
    Betaflight,
}

/// The Betaflight shortcut: not there, there, or the page it opens is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Betaflight {
    Hidden,
    Shown,
    Open,
}

/// The rail caption of the Betaflight shortcut: the code the firmware itself
/// reports, short enough for the 58 px item. Its tooltip spells the name out.
const BETAFLIGHT_CAPTION: &str = "BTFL";
const BETAFLIGHT_ICON: &str = "drone";
const BETAFLIGHT_KEY: u8 = 100;

/// Whether the connected aircraft runs Betaflight. The link carries the
/// stack name for as long as it is connected, so a link that is down shows
/// nothing, whatever it carried before.
pub fn betaflight_connected(state: &serde_json::Value) -> bool {
    state.get("connected").and_then(serde_json::Value::as_bool) == Some(true)
        && state.get("autopilot_stack").and_then(serde_json::Value::as_str) == Some("betaflight")
}

/// What the shortcut does: it is there while a Betaflight aircraft is connected,
/// or always when the operator asked for that, and shows as open while its page
/// is the one on screen. A page left open stays up if the aircraft goes away.
pub fn betaflight_slot(connected: bool, always: bool, page_open: bool) -> Betaflight {
    if !(connected || always) {
        Betaflight::Hidden
    } else if page_open {
        Betaflight::Open
    } else {
        Betaflight::Shown
    }
}

/// The rail's slots: the planner under HOME when it is on, a hairline there
/// otherwise; the Betaflight shortcut under SETUP while it applies; SET pushed
/// to the bottom by the spacer.
pub fn slots(mission: bool, betaflight: Betaflight) -> Vec<Slot> {
    let mut v = vec![Slot::Item(Page::Home)];
    if mission {
        v.extend([Slot::Item(Page::Mission), Slot::Divider]);
    } else {
        v.push(Slot::Divider);
    }
    v.push(Slot::Item(Page::Setup));
    if betaflight != Betaflight::Hidden {
        v.push(Slot::Betaflight);
    }
    v.extend([
        Slot::Item(Page::Analysis),
        Slot::Item(Page::Logs),
        Slot::Spacer,
        Slot::Item(Page::Settings),
    ]);
    v
}

/// Draws the rail into `rect`; returns the page clicked, if any.
/// The tooltip of SET: names the release while one is waiting.
pub fn settings_title(update: Option<&str>) -> String {
    match update {
        Some(v) => format!("Settings. Version {v} is available"),
        None => "Settings".to_string(),
    }
}

/// `--notice`: the colour of the dot that says a release is waiting.
fn notice() -> Color32 {
    if style::is_dark() {
        Color32::from_rgb(0xF5, 0x8A, 0x2B)
    } else {
        Color32::from_rgb(0xCE, 0x5E, 0x00)
    }
}

/// `update` is the release waiting, when there is one: SET gets a dot on the
/// icon's top right corner (`.nav-item.has-update::after`). While the
/// Betaflight page is open it holds the highlight instead of SETUP.
pub fn show(
    ui: &mut egui::Ui,
    rect: Rect,
    mission: bool,
    active: Page,
    labels: bool,
    update: Option<&str>,
    betaflight: Betaflight,
) -> Option<Click> {
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0, style::surface_1());
    painter.vline(
        rect.right() - 0.5,
        rect.y_range(),
        egui::Stroke::new(1.0, style::border()),
    );
    let slots = slots(mission, betaflight);
    // padding: 10px 0; gap: 4px; the spacer takes what is left.
    let fixed: f32 = slots
        .iter()
        .map(|s| match s {
            Slot::Item(_) | Slot::Betaflight => ITEM_H,
            Slot::Divider => 1.0 + 12.0,
            Slot::Spacer => 0.0,
        })
        .sum::<f32>()
        + 4.0 * (slots.len() as f32 - 1.0);
    let spacer = (rect.height() - 20.0 - fixed).max(0.0);
    let cx = rect.left() + (WIDTH - 1.0) / 2.0;
    let mut y = rect.top() + 10.0;
    let mut clicked = None;
    for slot in slots {
        match slot {
            Slot::Divider => {
                painter.hline(
                    egui::Rangef::new(cx - 17.0, cx + 17.0),
                    y + 6.5,
                    egui::Stroke::new(1.0, style::border_soft()),
                );
                y += 13.0;
            }
            Slot::Spacer => y += spacer,
            Slot::Item(_) | Slot::Betaflight => {
                let r = Rect::from_min_size(Pos2::new(cx - ITEM_W / 2.0, y), Vec2::new(ITEM_W, ITEM_H));
                let (key, label, icon, title, is_active) = match slot {
                    Slot::Item(page) => {
                        let (label, icon) = page.item();
                        let title = if page == Page::Settings {
                            settings_title(update)
                        } else {
                            String::new()
                        };
                        let taken = page == Page::Setup && betaflight == Betaflight::Open;
                        (page as u8, label, icon, title, page == active && !taken)
                    }
                    _ => (
                        BETAFLIGHT_KEY,
                        BETAFLIGHT_CAPTION,
                        BETAFLIGHT_ICON,
                        "Betaflight".to_string(),
                        betaflight == Betaflight::Open,
                    ),
                };
                let mut resp = ui.interact(r, ui.scope_id().with(("nav", key)), egui::Sense::click());
                if !title.is_empty() {
                    resp = resp.on_hover_tr(&title);
                }
                let (fill, icon_c, label_c) = if is_active {
                    (style::accent_dark(), style::text_1(), style::text_1())
                } else if resp.hovered() {
                    (style::surface_hover(), style::text_1(), style::text_2())
                } else {
                    (Color32::TRANSPARENT, style::text_2(), style::text_3())
                };
                painter.rect_filled(r, style::RADIUS_BTN, fill);
                if is_active {
                    // ::before: 3 x 26 at left -10px
                    let bar =
                        Rect::from_center_size(Pos2::new(r.left() - 10.0 + 1.5, r.center().y), Vec2::new(3.0, 26.0));
                    painter.rect_filled(bar, 2, style::accent());
                }
                // under 1280 the labels fold away and the icon is centred
                if labels {
                    let g = style::galley(ui, label, 9.0, Weight::W600, label_c, 0.7);
                    // the icon is 16 px: ui.icon writes it inline, over the 21 px
                    // of the stylesheet
                    let total = 16.0 + 4.0 + g.size().y;
                    let top = r.center().y - total / 2.0;
                    icons::paint(
                        ui,
                        icon,
                        Rect::from_min_size(Pos2::new(r.center().x - 8.0, top), Vec2::splat(16.0)),
                        icon_c,
                    );
                    painter.galley(Pos2::new(r.center().x - g.size().x / 2.0, top + 20.0), g, label_c);
                } else {
                    icons::paint(ui, icon, Rect::from_center_size(r.center(), Vec2::splat(16.0)), icon_c);
                }
                if slot == Slot::Item(Page::Settings) && update.is_some() {
                    crate::ui::kit::dot(
                        &painter,
                        Pos2::new(r.center().x + 6.0 + 3.5, r.top() + 6.0 + 3.5),
                        notice(),
                        true,
                    );
                }
                if resp.clicked() {
                    clicked = Some(match slot {
                        Slot::Item(page) => Click::Page(page),
                        _ => Click::Betaflight,
                    });
                }
                y += ITEM_H;
            }
        }
        y += 4.0;
    }
    clicked
}

/// A page that is not in the native interface yet: said plainly in the
/// look of the plugin tab's empty state, so it reads as the app's own.
pub fn placeholder(ui: &egui::Ui, rect: Rect, page: Page) {
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0, style::bg());
    let (_, icon) = page.item();
    let title = style::galley(ui, page.title(), 13.0, Weight::W600, style::text_1(), 0.0);
    let desc = crate::ui::kit::wrapped(
        ui,
        "This page is not in the native interface yet. It is still in the web interface.",
        11.0,
        style::text_3(),
        220.0,
    );
    let h = 52.0 + 14.0 + title.size().y + 14.0 + desc.size().y;
    let mut y = rect.center().y - h / 2.0;
    let tile = Rect::from_center_size(Pos2::new(rect.center().x, y + 26.0), Vec2::splat(52.0));
    {
        let r = tile;
        painter.rect_filled(r, 14, style::surface_2());
        painter.rect_stroke(r, 14, egui::Stroke::new(1.0, style::border()), egui::StrokeKind::Inside);
    }
    icons::paint(
        ui,
        icon,
        Rect::from_center_size(tile.center(), Vec2::splat(26.0)),
        style::accent(),
    );
    y += 52.0 + 14.0;
    painter.galley(
        Pos2::new(rect.center().x - title.size().x / 2.0, y),
        title.clone(),
        style::text_1(),
    );
    y += title.size().y + 14.0;
    painter.galley(
        Pos2::new(rect.center().x - desc.size().x / 2.0, y),
        desc,
        style::text_3(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rail_as_sidenav_builds_it() {
        use Page::*;
        assert_eq!(
            slots(false, Betaflight::Hidden),
            vec![
                Slot::Item(Home),
                Slot::Divider,
                Slot::Item(Setup),
                Slot::Item(Analysis),
                Slot::Item(Logs),
                Slot::Spacer,
                Slot::Item(Settings)
            ]
        );
        assert_eq!(
            slots(true, Betaflight::Hidden)[1..3],
            [Slot::Item(Mission), Slot::Divider]
        );
        assert_eq!(Settings.item(), ("SET", "settings"));
    }

    #[test]
    fn the_betaflight_shortcut_sits_under_setup_and_only_while_it_applies() {
        use Page::{Analysis, Setup};
        for shown in [Betaflight::Shown, Betaflight::Open] {
            let v = slots(false, shown);
            assert_eq!(v[2..4], [Slot::Item(Setup), Slot::Betaflight]);
            assert_eq!(v.len(), slots(false, Betaflight::Hidden).len() + 1);
        }
        assert!(!slots(true, Betaflight::Hidden).contains(&Slot::Betaflight));
        assert_eq!(
            slots(true, Betaflight::Shown)[3..5],
            [Slot::Item(Setup), Slot::Betaflight]
        );
        assert!(slots(false, Betaflight::Shown).contains(&Slot::Item(Analysis)));
    }

    #[test]
    fn the_shortcut_follows_the_aircraft_unless_the_operator_asked_for_it_always() {
        use Betaflight::{Hidden, Open, Shown};
        assert_eq!(
            betaflight_slot(false, false, false),
            Hidden,
            "the default: nothing without one"
        );
        assert_eq!(betaflight_slot(true, false, false), Shown);
        assert_eq!(betaflight_slot(true, false, true), Open);
        assert_eq!(
            betaflight_slot(false, true, false),
            Shown,
            "always: there with no aircraft"
        );
        assert_eq!(betaflight_slot(false, true, true), Open);
        assert_eq!(
            betaflight_slot(false, false, true),
            Hidden,
            "the aircraft left and nothing keeps it"
        );
    }

    #[test]
    fn a_betaflight_aircraft_is_one_the_link_says_is_connected_and_runs_betaflight() {
        use serde_json::json;
        assert!(betaflight_connected(
            &json!({"connected": true, "autopilot_stack": "betaflight"})
        ));
        assert!(
            !betaflight_connected(&json!({"connected": false, "autopilot_stack": "betaflight"})),
            "a link that dropped shows nothing, whatever it carried"
        );
        for stack in ["px4", "ardupilot", "generic", ""] {
            assert!(!betaflight_connected(
                &json!({"connected": true, "autopilot_stack": stack})
            ));
        }
        assert!(!betaflight_connected(&json!({"autopilot_stack": "betaflight"})));
        assert!(!betaflight_connected(&json!({})));
        assert!(!betaflight_connected(&serde_json::Value::Null));
    }

    #[test]
    fn set_names_the_release_that_is_waiting() {
        assert_eq!(settings_title(None), "Settings");
        assert_eq!(
            settings_title(Some("2026.12.01")),
            "Settings. Version 2026.12.01 is available"
        );
    }
}
