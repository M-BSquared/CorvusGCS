//! A terminal in a window of its own, over the whole app
//! (`python/src/js/term-window.js`, float-window.js, link.css `.term-win`): a 34 px
//! bar with the name, the address, a status and the tools, the terminal
//! under it, a grip at the corner. One window per session. The close button
//! puts the window away and the program keeps running; disconnect ends the
//! session.

use eframe::egui::{self, Pos2, Rect, Stroke, Vec2};
use serde_json::Value;

use crate::net::http::Server;
use crate::panel::ssh_address;
use crate::term::session::SshSession;
use crate::term::view::TermState;
use crate::ui::kit;
use crate::ui::style::{self, Weight};

const COLS: f32 = 80.0;
const ROWS: f32 = 20.0;
const MIN: Vec2 = Vec2::new(320.0, 160.0);

/// What the window asked for this frame.
#[derive(Debug, PartialEq)]
pub enum Out {
    None,
    Close,
    Disconnect,
}

/// One floating terminal.
pub struct TermWindow {
    pub name: String,
    title: String,
    host: String,
    session: SshSession,
    state: TermState,
    pos: Pos2,
    size: Vec2,
    maximized: Option<(Pos2, Vec2)>,
}

impl TermWindow {
    /// Opens a window on the session of the saved connection `conn`; the
    /// `nth` one open is shifted so they do not sit on each other.
    pub fn new(ctx: &egui::Context, server: &Server, conn: &Value, nth: usize) -> TermWindow {
        let name = conn.get("name").and_then(Value::as_str).unwrap_or("").to_string();
        let title = conn
            .get("title")
            .and_then(Value::as_str)
            .filter(|t| !t.is_empty())
            .unwrap_or(&name)
            .to_string();
        let cw = 7.2f32;
        let size = Vec2::new(
            (COLS * cw).ceil() + 21.0 + 1.0,
            ROWS * crate::term::view::CELL_H + 42.0 + 1.0,
        );
        let screen = ctx.content_rect();
        let pos = Pos2::new(
            (screen.center().x - size.x / 2.0 + 28.0 * nth as f32).max(8.0),
            (screen.center().y - size.y / 2.0 + 28.0 * nth as f32).max(8.0),
        );
        TermWindow {
            host: ssh_address(conn),
            session: SshSession::attach(ctx, server, &name, COLS as usize, ROWS as usize),
            name,
            title,
            state: TermState::default(),
            pos,
            size,
            maximized: None,
        }
    }

    pub fn show(&mut self, ctx: &egui::Context) -> Out {
        let screen = ctx.content_rect();
        let mut out = Out::None;
        let (pos, size) = if self.maximized.is_some() {
            (screen.min + Vec2::splat(12.0), screen.size() - Vec2::splat(24.0))
        } else {
            let size = self.size.max(MIN).min(screen.size());
            let pos = Pos2::new(
                self.pos.x.clamp(screen.left() - size.x + 80.0, screen.right() - 80.0),
                self.pos.y.clamp(screen.top(), screen.bottom() - 34.0),
            );
            (pos, size)
        };
        let rect = Rect::from_min_size(pos, size);
        let mut dragged = Vec2::ZERO;
        let mut resized = Vec2::ZERO;
        egui::Area::new(egui::Id::NULL.with(("termwin", &self.name)))
            .order(egui::Order::Foreground)
            .fixed_pos(rect.min)
            .constrain(false)
            .show(ctx, |ui| {
                ui.set_clip_rect(rect.expand(24.0));
                let painter = ui.painter().clone();
                let bg = painter.add(egui::Shape::Noop);
                let bar = Rect::from_min_size(rect.min, Vec2::new(rect.width(), 34.0));
                let body = Rect::from_min_max(Pos2::new(rect.left(), bar.bottom()), rect.max);
                crate::term::view::show(
                    ui,
                    Rect::from_min_max(
                        Pos2::new(body.left() + 1.0, body.top() + 1.0),
                        Pos2::new(body.right() - 1.0, body.bottom() - 1.0),
                    )
                    .shrink2(Vec2::new(0.0, 0.0)),
                    &mut self.session,
                    &mut self.state,
                    &self.name,
                    false,
                );
                painter.rect_filled(
                    bar,
                    egui::CornerRadius {
                        nw: 12,
                        ne: 12,
                        sw: 0,
                        se: 0,
                    },
                    style::glass(0.6),
                );
                painter.hline(bar.x_range(), bar.bottom() - 0.5, Stroke::new(1.0, style::border()));
                let bar_resp = ui.interact(bar, ui.scope_id().with("termbar"), egui::Sense::click_and_drag());
                if bar_resp.dragged() && self.maximized.is_none() {
                    dragged = bar_resp.drag_delta();
                }
                if bar_resp.double_clicked() {
                    self.toggle_max(rect);
                }
                crate::ui::icons::paint(
                    ui,
                    "square-terminal",
                    Rect::from_center_size(Pos2::new(bar.left() + 10.0 + 7.0, bar.center().y), Vec2::splat(14.0)),
                    style::text_2(),
                );
                let tx = bar.left() + 10.0 + 14.0 + 8.0;
                let tools_w = 3.0 * 24.0 + 2.0 + 6.0;
                let status_alive = !self.session.is_closed();
                let status = if status_alive { "CONNECTED" } else { "OFFLINE" };
                let sc = if status_alive {
                    style::healthy()
                } else {
                    style::text_3()
                };
                let sg = style::galley(ui, status, 10.0, Weight::W600, sc, 0.7);
                let sx = bar.right() - tools_w - 8.0 - sg.size().x;
                painter.galley(Pos2::new(sx, bar.center().y - sg.size().y / 2.0), sg, sc);
                kit::dot(&painter, Pos2::new(sx - 6.0 - 3.5, bar.center().y), sc, status_alive);
                let info = ui.painter_at(Rect::from_min_max(
                    Pos2::new(tx, bar.top()),
                    Pos2::new(sx - 20.0, bar.bottom()),
                ));
                let ng = style::galley(ui, &self.title, 12.0, Weight::W600, style::text_1(), 0.0);
                let hg = style::galley_mono(ui, &self.host, 10.0, style::text_3());
                info.galley(
                    Pos2::new(tx, bar.center().y - ng.size().y / 2.0),
                    ng.clone(),
                    style::text_1(),
                );
                info.galley(
                    Pos2::new(tx + ng.size().x + 8.0, bar.center().y - hg.size().y / 2.0 + 1.0),
                    hg,
                    style::text_3(),
                );
                let mut bx = bar.right() - 6.0 - 24.0;
                if kit::icon_btn(
                    ui,
                    Rect::from_min_size(Pos2::new(bx, bar.center().y - 12.0), Vec2::splat(24.0)),
                    "x",
                    "Close the window. What is running keeps running",
                    false,
                    "termclose",
                ) {
                    out = Out::Close;
                }
                bx -= 25.0;
                let max_icon = if self.maximized.is_some() {
                    "minimize-2"
                } else {
                    "maximize-2"
                };
                if kit::icon_btn(
                    ui,
                    Rect::from_min_size(Pos2::new(bx, bar.center().y - 12.0), Vec2::splat(24.0)),
                    max_icon,
                    if self.maximized.is_some() {
                        "Restore"
                    } else {
                        "Maximize"
                    },
                    false,
                    "termmax",
                ) {
                    self.toggle_max(rect);
                }
                bx -= 25.0;
                let dr = Rect::from_min_size(Pos2::new(bx, bar.center().y - 12.0), Vec2::splat(24.0));
                let hover = ui.rect_contains_pointer(dr);
                if kit::icon_btn(
                    ui,
                    dr,
                    "power",
                    "Disconnect. This stops what is running",
                    false,
                    "termdisc",
                ) {
                    out = Out::Disconnect;
                }
                if hover {
                    icons_hot(ui, dr);
                }
                if self.maximized.is_none() {
                    let grip = Rect::from_min_size(rect.right_bottom() - Vec2::splat(16.0), Vec2::splat(16.0));
                    let gr = ui.interact(grip, ui.scope_id().with("termgrip"), egui::Sense::drag());
                    if gr.hovered() || gr.dragged() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeNwSe);
                    }
                    if gr.dragged() {
                        resized = gr.drag_delta();
                    }
                    for k in [0.0, 5.0] {
                        painter.line_segment(
                            [
                                Pos2::new(rect.right() - 3.0 - k, rect.bottom() - 13.0 + 0.0),
                                Pos2::new(rect.right() - 13.0, rect.bottom() - 3.0 - k),
                            ],
                            Stroke::new(1.5, style::border_strong()),
                        );
                    }
                }
                painter.set(
                    bg,
                    egui::Shape::Vec(vec![
                        egui::Shape::Rect(style::shadow_pop().as_shape(rect, 12)),
                        egui::Shape::rect_filled(rect, 12, style::glass(0.97)),
                        egui::Shape::rect_stroke(
                            rect,
                            12,
                            Stroke::new(1.0, style::border_strong()),
                            egui::StrokeKind::Inside,
                        ),
                    ]),
                );
            });
        if self.maximized.is_none() {
            self.pos += dragged;
            self.size = (self.size + resized).max(MIN);
        }
        out
    }

    fn toggle_max(&mut self, rect: Rect) {
        self.maximized = match self.maximized {
            Some(_) => None,
            None => Some((rect.min, rect.size())),
        };
    }
}

fn icons_hot(ui: &egui::Ui, r: Rect) {
    crate::ui::icons::paint(
        ui,
        "power",
        Rect::from_center_size(r.center(), Vec2::splat(15.0)),
        style::critical(),
    );
}
