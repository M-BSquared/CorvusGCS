use super::*;

impl Control {
    /// One stick: a base, a knob that follows the pointer, a caption.
    pub(super) fn stick(&self, ui: &egui::Ui, at: Pos2, index: usize, caption: &str) -> Stick {
        let base = Rect::from_min_size(at, Vec2::splat(SIZE));
        let resp = ui.interact(base, ui.scope_id().with(("stick", index)), egui::Sense::drag());
        let active = resp.is_pointer_button_down_on() || resp.dragged();
        let mut value = Stick::CENTRE;
        if active {
            if let Some(p) = resp.interact_pointer_pos() {
                let radius = f64::from(SIZE) / 2.0;
                let nx = f64::from(p.x - base.center().x) / radius;
                let ny = f64::from(base.center().y - p.y) / radius;
                let c = model::clamp_to_circle(nx, ny);
                value = Stick {
                    x: model::apply_deadzone(c.x),
                    y: model::apply_deadzone(c.y),
                };
            }
        }
        let painter = ui.painter();
        painter.circle_filled(base.center(), SIZE / 2.0, style::input(0.5));
        painter.circle_stroke(
            base.center(),
            SIZE / 2.0 - 0.5,
            Stroke::new(1.0, if active { style::accent() } else { style::border() }),
        );
        if active {
            painter.circle_stroke(base.center(), SIZE / 2.0 + 1.0, Stroke::new(2.0, style::accent_dim()));
        }
        let hair = Stroke::new(1.0, style::border_soft());
        painter.hline((base.left() + 1.0)..=(base.right() - 1.0), base.center().y, hair);
        painter.vline(base.center().x, (base.top() + 1.0)..=(base.bottom() - 1.0), hair);
        let travel = (SIZE - KNOB) / 2.0;
        let knob = base.center() + Vec2::new(value.x as f32 * travel, -value.y as f32 * travel);
        painter.circle_filled(knob + Vec2::new(0.0, 2.0), KNOB / 2.0, Color32::from_black_alpha(46));
        painter.circle_filled(
            knob,
            KNOB / 2.0,
            if active { style::accent() } else { style::surface_3() },
        );
        painter.circle_stroke(
            knob,
            KNOB / 2.0 - 0.5,
            Stroke::new(
                1.0,
                if active {
                    style::accent()
                } else {
                    style::border_strong()
                },
            ),
        );
        let g = style::galley(ui, &caption.to_uppercase(), 8.5, Weight::W600, style::text_3(), 0.7);
        painter.galley(
            Pos2::new(base.center().x - g.size().x / 2.0, base.bottom() + 4.0),
            g,
            style::text_3(),
        );
        value
    }

    /// The gamepad block, its bottom edge at `at.y`: two rings, the status
    /// line, the arm bar.
    pub(super) fn gamepad(&self, ui: &egui::Ui, at: Pos2, now_ms: f64) {
        let present = self.pad.is_some();
        let painter = ui.painter();
        let dot = if present { style::accent() } else { style::text_3() };
        let rings_w = GP_RING * 2.0 + 10.0;
        let x0 = at.x + (GP_W - rings_w) / 2.0;
        let (caption, status) = self.gamepad_text(ui);
        let mut y = at.y - self.gamepad_height(ui);
        let at = Pos2::new(at.x, y);
        for (i, v) in [self.shown_left, self.shown_right].iter().enumerate() {
            let c = Pos2::new(x0 + GP_RING / 2.0 + i as f32 * (GP_RING + 10.0), y + GP_RING / 2.0);
            painter.circle_filled(c, GP_RING / 2.0, style::input(0.5));
            painter.circle_stroke(c, GP_RING / 2.0 - 0.5, Stroke::new(1.0, style::border()));
            let travel = (GP_RING - GP_DOT) / 2.0;
            painter.circle_filled(
                c + Vec2::new(v.x as f32 * travel, -v.y as f32 * travel),
                GP_DOT / 2.0,
                dot,
            );
        }
        y += GP_RING + 4.0;
        painter.galley(
            Pos2::new(at.x + (GP_W - caption.size().x) / 2.0, y),
            caption.clone(),
            style::text_3(),
        );
        y += caption.size().y + 4.0;
        let sc = if !present { style::text_3() } else { style::text_2() };
        painter.galley(Pos2::new(at.x + (GP_W - status.size().x) / 2.0, y), status.clone(), sc);
        y += status.size().y + 4.0;
        let bar = Rect::from_min_size(Pos2::new(at.x, y), Vec2::new(GP_W, 3.0));
        painter.rect_filled(bar, 2, style::border_soft());
        let p = self.gesture.progress(now_ms, self.armed) as f32;
        if p > 0.0 {
            painter.rect_filled(
                Rect::from_min_size(bar.min, Vec2::new(GP_W * p, 3.0)),
                2,
                if self.armed { style::accent() } else { style::warning() },
            );
        }
    }

    /// The key clusters, bottom aligned to the right of what is before them.
    pub(super) fn keys(&mut self, ui: &egui::Ui, x: f32, bottom: f32, right: f32) {
        let cluster_w = KEY * 3.0 + 8.0;
        let n = usize::from(self.cfg.keys) + usize::from(self.cfg.wasd);
        let total = cluster_w * n as f32 + 8.0 * (n.saturating_sub(1)) as f32;
        let mut cx = (right - total).max(x);
        let top = bottom - Control::keys_height();
        let mut clusters: Vec<Cluster> = Vec::new();
        if self.cfg.wasd {
            clusters.push((
                "THR \u{b7} YAW",
                [
                    ("thrust_up", "W", None),
                    ("yaw_left", "A", None),
                    ("thrust_down", "S", None),
                    ("yaw_right", "D", None),
                ],
            ));
        }
        if self.cfg.keys {
            clusters.push((
                "ARROW KEYS",
                [
                    ("up", "", Some("arrow-up")),
                    ("left", "", Some("arrow-left")),
                    ("down", "", Some("arrow-down")),
                    ("right", "", Some("arrow-right")),
                ],
            ));
        }
        let mut pointer_held = Held::default();
        for (caption, keys) in clusters {
            // The inverted T: up in the middle of the top row, then left,
            // down, right along the bottom.
            let cells = [(1, 0), (0, 1), (1, 1), (2, 1)];
            for ((dir, letter, icon), (col, row)) in keys.iter().zip(cells) {
                let r = Rect::from_min_size(
                    Pos2::new(cx + col as f32 * (KEY + 4.0), top + row as f32 * (KEY + 4.0)),
                    Vec2::splat(KEY),
                );
                let resp = ui.interact(r, ui.scope_id().with(("key", *dir)), egui::Sense::click_and_drag());
                let down = resp.is_pointer_button_down_on();
                if down {
                    match *dir {
                        "up" => pointer_held.up = true,
                        "down" => pointer_held.down = true,
                        "left" => pointer_held.left = true,
                        "right" => pointer_held.right = true,
                        "thrust_up" => pointer_held.thrust_up = true,
                        "thrust_down" => pointer_held.thrust_down = true,
                        "yaw_left" => pointer_held.yaw_left = true,
                        _ => pointer_held.yaw_right = true,
                    }
                }
                let lit = down || self.is_held(dir);
                let (fill, color, stroke) = if lit {
                    let c = if self.boost { style::warning() } else { style::accent() };
                    (c, style::text_inverted(), c)
                } else {
                    (
                        style::input(0.5),
                        if resp.hovered() {
                            style::text_1()
                        } else {
                            style::text_2()
                        },
                        if resp.hovered() {
                            style::border_strong()
                        } else {
                            style::border()
                        },
                    )
                };
                let r = if lit { r.translate(Vec2::new(0.0, 1.0)) } else { r };
                let painter = ui.painter();
                painter.rect_filled(r, style::RADIUS_BTN, fill);
                painter.rect_stroke(r, style::RADIUS_BTN, Stroke::new(1.0, stroke), egui::StrokeKind::Inside);
                if let Some(icon) = icon {
                    icons::paint(ui, icon, Rect::from_center_size(r.center(), Vec2::splat(13.0)), color);
                } else {
                    let g = style::galley_mono(ui, letter, 11.0, color);
                    painter.galley(r.center() - g.size() / 2.0, g, color);
                }
            }
            let c = style::galley(
                ui,
                caption,
                8.5,
                Weight::W600,
                if self.boost { style::warning() } else { style::text_3() },
                0.7,
            );
            ui.painter().galley(
                Pos2::new(cx + cluster_w / 2.0 - c.size().x / 2.0, top + KEY * 2.0 + 4.0 + 4.0),
                c,
                if self.boost { style::warning() } else { style::text_3() },
            );
            cx += cluster_w + 8.0;
        }
        // A key held by the pointer flies like one held by the keyboard.
        self.pointer = if self.collapsed() {
            Held::default()
        } else {
            pointer_held
        };
    }

    pub(super) fn is_held(&self, dir: &str) -> bool {
        let h = or_held(self.kbd, Held::default());
        match dir {
            "up" => h.up,
            "down" => h.down,
            "left" => h.left,
            "right" => h.right,
            "thrust_up" => h.thrust_up,
            "thrust_down" => h.thrust_down,
            "yaw_left" => h.yaw_left,
            _ => h.yaw_right,
        }
    }
}
