//! The left rail (src/js/sidenav.js renderLeftNav, `.left-nav`,
//! `.nav-item`): HOME, MISSION when the config turns the planner on (a
//! hairline otherwise), SETUP, ANALYSIS, LOGS, and SET at the bottom.

use eframe::egui::{self, Color32, Pos2, Rect, Vec2};

use crate::icons;
use crate::style::{self, Weight};

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
    Divider,
    Spacer,
}

/// The rail's slots: the planner under HOME when it is on, a hairline there
/// otherwise; SET pushed to the bottom by the spacer.
pub fn slots(mission: bool) -> Vec<Slot> {
    let mut v = vec![Slot::Item(Page::Home)];
    if mission {
        v.extend([Slot::Item(Page::Mission), Slot::Divider]);
    } else {
        v.push(Slot::Divider);
    }
    v.extend([
        Slot::Item(Page::Setup),
        Slot::Item(Page::Analysis),
        Slot::Item(Page::Logs),
        Slot::Spacer,
        Slot::Item(Page::Settings),
    ]);
    v
}

/// Draws the rail into `rect`; returns the page clicked, if any.
pub fn show(ui: &mut egui::Ui, rect: Rect, mission: bool, active: Page) -> Option<Page> {
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0, style::surface_1());
    painter.vline(
        rect.right() - 0.5,
        rect.y_range(),
        egui::Stroke::new(1.0, style::border()),
    );
    let slots = slots(mission);
    // padding: 10px 0; gap: 4px; the spacer takes what is left.
    let fixed: f32 = slots
        .iter()
        .map(|s| match s {
            Slot::Item(_) => ITEM_H,
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
            Slot::Item(page) => {
                let r = Rect::from_min_size(Pos2::new(cx - ITEM_W / 2.0, y), Vec2::new(ITEM_W, ITEM_H));
                let title = if page == Page::Settings { "Settings" } else { "" };
                let mut resp = ui.interact(r, ui.scope_id().with(("nav", page as u8)), egui::Sense::click());
                if !title.is_empty() {
                    resp = resp.on_hover_text(title);
                }
                let is_active = page == active;
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
                let (label, icon) = page.item();
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
                if resp.clicked() {
                    clicked = Some(page);
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
    let desc = crate::kit::wrapped(
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
            slots(false),
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
        assert_eq!(slots(true)[1..3], [Slot::Item(Mission), Slot::Divider]);
        assert_eq!(Settings.item(), ("SET", "settings"));
    }
}
