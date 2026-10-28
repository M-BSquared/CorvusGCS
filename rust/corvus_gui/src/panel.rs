//! The right panel (index.html `aside.right-panel`, panel.css): the
//! collapse handle, the resize grip, the tab strip and the four tabs.
//!
//! LINK and CONSOLE are whole. SSH lists the connections the backend knows,
//! as panel.js draws them; the terminal behind CONNECT is its own step.
//! PLUGINS shows the tab's empty state: the plugins in plugins/ are
//! JavaScript, which this app does not run, and Rust plugins come later.

use eframe::egui::{self, Color32, Pos2, Rect, Stroke, Vec2};
use serde_json::Value;

use crate::bg::Pending;
use crate::console::ConsoleTab;
use crate::http::{self, Server};
use crate::icons;
use crate::kit::{self, Button, Variant};
use crate::link::{self, LinkTab};
use crate::style::{self, Weight};

/// `--panel-w`, and the grip's range.
pub const DEFAULT_WIDTH: f32 = 380.0;
const MIN_WIDTH: f32 = 320.0;
const MAX_WIDTH: f32 = 560.0;
/// `--panel-w-collapsed`: the handle alone.
pub const HANDLE: f32 = 24.0;

/// A tab of the panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Link,
    Console,
    Ssh,
    Plugins,
}

impl Tab {
    fn caption(self) -> &'static str {
        match self {
            Tab::Link => "LINK",
            Tab::Console => "CONSOLE",
            Tab::Ssh => "SSH",
            Tab::Plugins => "PLUGINS",
        }
    }
}

/// The tabs shown: CONSOLE and SSH can be switched off in the config
/// (`ui.console_tab`, `ui.ssh_tab`), LINK and PLUGINS cannot.
pub fn tabs(console: bool, ssh: bool) -> Vec<Tab> {
    let mut v = vec![Tab::Link];
    if console {
        v.push(Tab::Console);
    }
    if ssh {
        v.push(Tab::Ssh);
    }
    v.push(Tab::Plugins);
    v
}

/// The panel's state.
pub struct Panel {
    server: Server,
    pub width: f32,
    pub collapsed: bool,
    active: Tab,
    pub console_tab: bool,
    pub ssh_tab: bool,
    link: LinkTab,
    console: ConsoleTab,
    ssh_list: Option<Value>,
    ssh_loading: Option<Pending<Result<Value, String>>>,
}

impl Panel {
    pub fn new(server: Server, ctx: &egui::Context, config: &Value) -> Panel {
        let flag = |k: &str| {
            config
                .get("ui")
                .and_then(|u| u.get(k))
                .and_then(Value::as_bool)
                .unwrap_or(true)
        };
        Panel {
            link: LinkTab::new(server.clone(), ctx),
            console: ConsoleTab::new(server.clone(), ctx),
            server,
            width: DEFAULT_WIDTH,
            collapsed: false,
            active: Tab::Link,
            console_tab: flag("console_tab"),
            ssh_tab: flag("ssh_tab"),
            ssh_list: None,
            ssh_loading: None,
        }
    }

    /// The width the panel takes from the window now.
    pub fn current_width(&self) -> f32 {
        if self.collapsed {
            HANDLE
        } else {
            self.width
        }
    }

    /// Ends the panel's streams.
    pub fn shut_down(&mut self) {
        self.console.shut_down();
    }

    /// Draws the panel into `rect` (its full current width).
    pub fn show(&mut self, ui: &mut egui::Ui, rect: Rect, state: &Value) {
        let painter = ui.painter_at(rect);
        // .panel-handle
        let handle = Rect::from_min_size(rect.min, Vec2::new(HANDLE, rect.height()));
        let hresp = ui
            .interact(handle, ui.scope_id().with("handle"), egui::Sense::click())
            .on_hover_text(if self.collapsed {
                "Expand panel"
            } else {
                "Collapse panel"
            });
        painter.rect_filled(
            handle,
            0,
            if hresp.hovered() {
                style::surface_hover()
            } else {
                style::surface_2()
            },
        );
        painter.vline(handle.left() + 0.5, handle.y_range(), Stroke::new(1.0, style::border()));
        painter.vline(
            handle.right() - 0.5,
            handle.y_range(),
            Stroke::new(1.0, style::border()),
        );
        let icon = if self.collapsed {
            "chevron-left"
        } else {
            "chevron-right"
        };
        icons::paint(
            ui,
            icon,
            Rect::from_center_size(handle.center(), Vec2::splat(16.0)),
            if hresp.hovered() {
                style::accent_bright()
            } else {
                style::text_3()
            },
        );
        if hresp.clicked() {
            self.collapsed = !self.collapsed;
        }
        if self.collapsed {
            return;
        }
        let body = Rect::from_min_max(Pos2::new(handle.right(), rect.top()), rect.max);
        painter.rect_filled(body, 0, style::surface_1());

        // .panel-grip: 7 px over the handle's right edge
        let grip = Rect::from_min_size(
            Pos2::new(handle.right() - 3.0, rect.top()),
            Vec2::new(7.0, rect.height()),
        );
        let gresp = ui
            .interact(grip, ui.scope_id().with("grip"), egui::Sense::click_and_drag())
            .on_hover_text("Drag to resize, double click to reset")
            .on_hover_cursor(egui::CursorIcon::ResizeColumn);
        if gresp.dragged() {
            self.width = (self.width - gresp.drag_delta().x).clamp(MIN_WIDTH, MAX_WIDTH);
        }
        if gresp.double_clicked() {
            self.width = DEFAULT_WIDTH;
        }
        if gresp.hovered() || gresp.dragged() {
            ui.painter().vline(
                grip.left() + 4.0,
                grip.y_range(),
                Stroke::new(2.0, Color32::from_rgba_unmultiplied(27, 31, 38, 178)),
            );
        }

        // .panel-tabs-wrap: padding 8 12 0, a rule under it
        let shown = tabs(self.console_tab, self.ssh_tab);
        if !shown.contains(&self.active) {
            self.active = Tab::Link;
        }
        let tab_h = 9.0 * 2.0 + 11.0 * 1.21 + 2.0;
        let strip = Rect::from_min_size(body.min, Vec2::new(body.width(), 8.0 + tab_h + 1.0));
        painter.hline(strip.x_range(), strip.bottom() - 0.5, Stroke::new(1.0, style::border()));
        let galleys: Vec<_> = shown
            .iter()
            .map(|t| style::galley(ui, t.caption(), 11.0, Weight::W600, style::text_3(), 0.9))
            .collect();
        let natural: f32 = galleys.iter().map(|g| g.size().x + 20.0).sum::<f32>() + 2.0 * (shown.len() as f32 - 1.0);
        let extra = ((strip.width() - 24.0 - natural) / shown.len() as f32).max(0.0);
        let mut x = strip.left() + 12.0;
        for (i, t) in shown.iter().enumerate() {
            let w = galleys[i].size().x + 20.0 + extra;
            let r = Rect::from_min_size(Pos2::new(x, strip.top() + 8.0), Vec2::new(w, tab_h));
            let resp = ui.interact(r, ui.scope_id().with(("tab", i)), egui::Sense::click());
            let active = *t == self.active;
            let color = if active {
                style::accent()
            } else if resp.hovered() {
                style::text_1()
            } else {
                style::text_3()
            };
            if resp.hovered() && !active {
                painter.rect_filled(
                    r,
                    egui::CornerRadius {
                        nw: 6,
                        ne: 6,
                        sw: 0,
                        se: 0,
                    },
                    style::surface_hover(),
                );
            }
            if active {
                painter.hline(r.x_range(), r.bottom() - 1.0, Stroke::new(2.0, style::accent()));
            }
            let g = style::galley(ui, t.caption(), 11.0, Weight::W600, color, 0.9);
            painter.galley(
                Pos2::new(r.center().x - g.size().x / 2.0, r.center().y - 1.0 - g.size().y / 2.0),
                g,
                color,
            );
            if resp.clicked() {
                self.active = *t;
                if *t == Tab::Ssh {
                    self.load_ssh(ui.ctx());
                }
            }
            x += w + 2.0;
        }

        let content = Rect::from_min_max(Pos2::new(body.left(), strip.bottom()), body.max);
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(content));
        child.set_clip_rect(content);
        match self.active {
            Tab::Link => self.link.show(&mut child, content, state),
            Tab::Console => self.console.show(&mut child, content),
            Tab::Ssh => self.ssh(&mut child, content),
            Tab::Plugins => plugins_empty(&child, content),
        }
    }

    fn load_ssh(&mut self, ctx: &egui::Context) {
        let server = self.server.clone();
        self.ssh_loading = Some(Pending::spawn(ctx, move || {
            http::get_json(&server, "/api/ssh/connections")
        }));
    }

    fn ssh(&mut self, ui: &mut egui::Ui, rect: Rect) {
        if self.ssh_list.is_none() && self.ssh_loading.is_none() {
            self.load_ssh(ui.ctx());
        }
        if let Some(r) = self.ssh_loading.as_ref().and_then(Pending::take) {
            self.ssh_loading = None;
            self.ssh_list = Some(r.unwrap_or_else(|_| serde_json::json!({ "failed": true })));
        }
        let header = link::section_header(ui, rect, "SSH Access");
        let add = Rect::from_min_size(
            Pos2::new(header.right() - 12.0 - 28.0, header.center().y - 14.0),
            Vec2::splat(28.0),
        );
        let painter = ui.painter().clone();
        // Adding a connection opens a dialog, which comes with the terminal.
        icons::paint(
            ui,
            "plus",
            Rect::from_center_size(add.center(), Vec2::splat(15.0)),
            style::text_3().gamma_multiply(0.45),
        );
        ui.interact(add, ui.scope_id().with("sshadd"), egui::Sense::hover())
            .on_hover_text("Add connection (not in this build yet)");
        let mut y = header.bottom() + 12.0;
        let x = rect.left() + 12.0;
        let w = rect.width() - 24.0;
        let list = self.ssh_list.clone().unwrap_or(Value::Null);
        if list.get("failed").is_some() {
            let card = Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, 13.0 * 2.0 + 18.0));
            {
                let r = card;
                painter.rect_filled(r, style::RADIUS, style::surface_2());
                painter.rect_stroke(
                    r,
                    style::RADIUS,
                    Stroke::new(1.0, style::border()),
                    egui::StrokeKind::Inside,
                );
            }
            let g = style::galley(
                ui,
                "Could not load connections.",
                12.0,
                Weight::W400,
                style::text_3(),
                0.0,
            );
            painter.galley(card.min + Vec2::new(13.0, 13.0), g, style::text_3());
            y = card.bottom() + 10.0;
        }
        let devices = list
            .get("connections")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for (i, dev) in devices.iter().enumerate() {
            let connected = dev.get("connected") == Some(&Value::Bool(true));
            let card = Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, 13.0 * 2.0 + 34.0 + 10.0 + 38.0));
            {
                let r = card;
                painter.rect_filled(r, style::RADIUS, style::surface_2());
                painter.rect_stroke(
                    r,
                    style::RADIUS,
                    Stroke::new(1.0, style::border()),
                    egui::StrokeKind::Inside,
                );
            }
            let inner = card.shrink(13.0);
            let tile = Rect::from_min_size(inner.min, Vec2::splat(34.0));
            painter.rect_filled(tile, style::RADIUS_BTN, style::surface_3());
            icons::paint(
                ui,
                "server",
                Rect::from_center_size(tile.center(), Vec2::splat(17.0)),
                style::text_2(),
            );
            let name = dev
                .get("title")
                .or_else(|| dev.get("name"))
                .and_then(Value::as_str)
                .unwrap_or("");
            let ng = style::galley(ui, name, 13.0, Weight::W600, style::text_1(), 0.0);
            let hg = style::galley_mono(ui, &ssh_address(dev), 11.0, style::text_2());
            let status = if connected { "CONNECTED" } else { "OFFLINE" };
            let sc = if connected { style::healthy() } else { style::text_3() };
            let sg = style::galley(ui, status, 10.0, Weight::W600, sc, 0.7);
            let sx = inner.right() - sg.size().x;
            painter.galley(Pos2::new(sx, tile.center().y - sg.size().y / 2.0), sg, sc);
            kit::dot(&painter, Pos2::new(sx - 6.0 - 3.5, tile.center().y), sc, connected);
            let text_clip = Rect::from_min_max(
                Pos2::new(tile.right() + 10.0, tile.top()),
                Pos2::new(sx - 20.0, tile.bottom()),
            );
            let tp = ui.painter_at(text_clip);
            let th = ng.size().y + 1.0 + hg.size().y;
            tp.galley(
                Pos2::new(text_clip.left(), tile.center().y - th / 2.0),
                ng.clone(),
                style::text_1(),
            );
            tp.galley(
                Pos2::new(text_clip.left(), tile.center().y - th / 2.0 + ng.size().y + 1.0),
                hg,
                style::text_2(),
            );
            // CONNECT and the two square buttons: the terminal is its own step.
            let row_y = tile.bottom() + 10.0;
            let b = Button::new(if connected { "OPEN TERMINAL" } else { "CONNECT" }, Variant::Primary)
                .enabled(false)
                .title("The SSH terminal is not in this build yet");
            let r = Rect::from_min_size(
                Pos2::new(inner.left(), row_y),
                Vec2::new(inner.width() - 2.0 * (34.0 + 8.0), b.height()),
            );
            let bh = b.height();
            b.show(ui, r, ("sshconnect", i));
            for (j, icon) in ["chevron-right", "pencil"].into_iter().enumerate() {
                let ir = Rect::from_min_size(
                    Pos2::new(r.right() + 8.0 + j as f32 * 42.0, row_y + (bh - 34.0) / 2.0),
                    Vec2::splat(34.0),
                );
                icons::paint(
                    ui,
                    icon,
                    Rect::from_center_size(ir.center(), Vec2::splat(15.0)),
                    style::text_3().gamma_multiply(0.45),
                );
            }
            y = card.bottom() + 10.0;
        }
        if self.ssh_list.is_some() {
            let b = Button::new("ADD CONNECTION", Variant::Secondary)
                .icon("plus")
                .enabled(false)
                .title("Adding a connection is not in this build yet");
            let r = Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, b.height()));
            b.show(ui, r, "sshaddblock");
        }
    }
}

/// `user@host:port`, as panel.js sshAddress writes it.
pub fn ssh_address(conn: &Value) -> String {
    let host = conn.get("host").and_then(Value::as_str).unwrap_or("");
    let port = match conn.get("port") {
        Some(Value::Number(n)) if n.as_f64() != Some(0.0) => format!(":{n}"),
        Some(Value::String(s)) if !s.is_empty() => format!(":{s}"),
        _ => String::new(),
    };
    let user = conn
        .get("username")
        .and_then(Value::as_str)
        .filter(|u| !u.is_empty())
        .map(|u| format!("{u}@"))
        .unwrap_or_default();
    format!("{user}{host}{port}")
}

/// The PLUGINS tab with nothing in it (`.future-empty`).
fn plugins_empty(ui: &egui::Ui, rect: Rect) {
    let painter = ui.painter();
    let title = style::galley(ui, "Tools & Plugins", 13.0, Weight::W600, style::text_1(), 0.0);
    let desc = kit::wrapped(ui, "Extensions and tools plug in here.", 11.0, style::text_3(), 220.0);
    let h = 52.0 + 14.0 + title.size().y + 14.0 + desc.size().y;
    let mut y = rect.center().y - h / 2.0;
    let tile = Rect::from_center_size(Pos2::new(rect.center().x, y + 26.0), Vec2::splat(52.0));
    {
        let r = tile;
        painter.rect_filled(r, 14, style::surface_2());
        painter.rect_stroke(r, 14, Stroke::new(1.0, style::border()), egui::StrokeKind::Inside);
    }
    icons::paint(
        ui,
        "puzzle",
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
    use serde_json::json;

    #[test]
    fn the_tabs_follow_the_config() {
        assert_eq!(tabs(true, true), vec![Tab::Link, Tab::Console, Tab::Ssh, Tab::Plugins]);
        assert_eq!(tabs(false, true), vec![Tab::Link, Tab::Ssh, Tab::Plugins]);
        assert_eq!(tabs(false, false), vec![Tab::Link, Tab::Plugins]);
    }

    #[test]
    fn ssh_addresses() {
        assert_eq!(
            ssh_address(&json!({"host": "10.0.0.2", "username": "pi", "port": 2222})),
            "pi@10.0.0.2:2222"
        );
        assert_eq!(ssh_address(&json!({"host": "jetson"})), "jetson");
    }
}
