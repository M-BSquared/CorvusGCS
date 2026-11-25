//! A module's view on the screen: what of the operator's input is its business
//! goes in, and the triangles that come out are painted with the program's
//! renderer.
//!
//! The module gets the pointer only where its view is not covered by something
//! of the program, and the keys only once it was opened or pressed, until the
//! operator presses elsewhere. While a text field
//! of its has the keyboard, a stand-in widget holds the program's focus: the
//! keys that fly the vehicle are quiet then, as they are for every text field.

use std::collections::HashMap;

use corvus_api::bridge;
use corvus_api::ui::frost;
use corvus_api::wire::{self, Maybe};
use eframe::egui::{self, Id, LayerId, Order, Rect, Sense};

const TEXTURES_MAX: usize = 512;
const OVERLAYS_MAX: usize = 32;
const PAINT_MAX: usize = 20_000;
const COPY_MAX: usize = 4 * 1024 * 1024;

/// The widest texture the window's renderer takes.
fn max_texture_side(ctx: &egui::Context) -> u32 {
    u32::try_from(ctx.input(|i| i.max_texture_side)).unwrap_or(2048)
}

pub struct View {
    plugin: &'static str,
    textures: HashMap<u64, egui::TextureHandle>,
    /// The module's popups of the last frame, which lie over everything.
    overlays: Vec<Rect>,
    wants_keyboard: bool,
    /// The keys are the module's business: the operator's last press was on it, or it has just
    /// been opened and has not been pressed beside.
    keyboard: bool,
    /// The view is about to be shown for the first time since it was closed.
    fresh: bool,
    /// A press beside the module is being told to it, so a field of its lets go.
    leaving: bool,
    /// A button pressed on the module is still down: it keeps the pointer.
    captured: bool,
    /// The module believes the pointer is over it.
    pointer_in: bool,
    over: bool,
    /// The operator clicked or confirmed something in this frame.
    acted: bool,
}

impl View {
    pub fn new(plugin: &'static str) -> View {
        View {
            plugin,
            textures: HashMap::new(),
            overlays: Vec::new(),
            wants_keyboard: false,
            keyboard: false,
            fresh: true,
            leaving: false,
            captured: false,
            pointer_in: false,
            over: false,
            acted: false,
        }
    }

    pub fn plugin(&self) -> &'static str {
        self.plugin
    }

    fn id(&self) -> Id {
        Id::unique(("plugin-view", self.plugin))
    }

    /// Where the module's popups are.
    #[cfg(test)]
    pub fn overlays(&self) -> &[Rect] {
        &self.overlays
    }

    /// The view is gone. The textures stay: the module's egui goes on with them.
    pub fn closed(&mut self) {
        self.overlays.clear();
        self.wants_keyboard = false;
        self.keyboard = false;
        self.fresh = true;
        self.leaving = false;
        self.captured = false;
        self.pointer_in = false;
    }

    /// This frame's input for the module.
    pub fn input(&mut self, ui: &mut egui::Ui, area: Rect) -> wire::ShowInput {
        let ctx = ui.ctx().clone();
        let id = self.id();
        let clip = ui.clip_rect();
        // Opening a view is meant for it: a field it puts the cursor into by itself gets the
        // keys, until the operator presses somewhere else.
        if std::mem::take(&mut self.fresh) {
            self.keyboard = true;
        }
        let stand_in = ui.interact(clip, id, Sense::click_and_drag());
        let mut over = stand_in.contains_pointer();
        for (n, rect) in self.overlays.iter().enumerate() {
            egui::Area::new(id.with(("over", n)))
                .order(Order::Foreground)
                .fixed_pos(rect.min)
                .constrain(false)
                .show(&ctx, |ui| {
                    over |= ui
                        .allocate_exact_size(rect.size(), Sense::click_and_drag())
                        .1
                        .contains_pointer();
                });
        }
        self.over = over;
        let popups = !self.overlays.is_empty();

        let (raw, time, predicted_dt, focused, modifiers, any_down) = ctx.input(|i| {
            (
                i.events.clone(),
                i.time,
                i.predicted_dt,
                i.focused,
                i.modifiers,
                i.pointer.any_down(),
            )
        });
        // A field of the program's that has the keyboard keeps it.
        let elsewhere = ctx.memory(|m| m.focused()).is_some_and(|f| f != id);
        if elsewhere {
            self.keyboard = false;
        }
        self.acted = false;
        let mut events = Vec::new();
        for event in &raw {
            let for_module = match event {
                egui::Event::PointerButton { pressed: true, .. } => {
                    self.keyboard = over;
                    self.captured |= over;
                    // A press elsewhere takes the cursor out of a field of the module's, which
                    // may only then take what was typed into it.
                    self.leaving = !over && self.wants_keyboard;
                    // A press beside a popup closes it.
                    over || popups || self.leaving
                }
                egui::Event::PointerButton { pressed: false, .. } => {
                    let mine = self.captured || over || popups || std::mem::take(&mut self.leaving);
                    self.acted |= self.captured || over;
                    mine
                }
                egui::Event::PointerMoved(_) => over || self.captured || popups,
                egui::Event::PointerGone => self.pointer_in,
                egui::Event::MouseWheel { .. } | egui::Event::Zoom(_) => over,
                egui::Event::Touch { .. } => over || self.captured,
                egui::Event::Key { key, pressed, .. } => {
                    self.acted |= self.keyboard && *pressed && *key == egui::Key::Enter;
                    self.keyboard
                }
                egui::Event::Text(_)
                | egui::Event::Paste(_)
                | egui::Event::Copy
                | egui::Event::Cut
                | egui::Event::Ime(_) => self.keyboard,
                _ => false,
            };
            if !for_module {
                continue;
            }
            if let Some(e) = bridge::event_to(event) {
                match event {
                    egui::Event::PointerMoved(_) | egui::Event::PointerButton { .. } => self.pointer_in = true,
                    egui::Event::PointerGone => self.pointer_in = false,
                    _ => {}
                }
                events.push(Maybe::Known(e));
            }
        }
        if !any_down {
            self.captured = false;
        }
        if self.pointer_in && !over && !self.captured && !popups {
            self.pointer_in = false;
            events.push(Maybe::Known(wire::Event::PointerGone {}));
        }
        self.hold_focus(ui, id);

        wire::ShowInput {
            screen: bridge::rect_to(ctx.content_rect()),
            area: bridge::rect_to(area),
            clip: bridge::rect_to(clip),
            pixels_per_point: ctx.pixels_per_point(),
            time,
            predicted_dt,
            focused,
            modifiers: bridge::modifiers_to(modifiers),
            events,
            textures_lost: false,
            max_texture_side: max_texture_side(&ctx),
        }
    }

    fn hold_focus(&self, ui: &egui::Ui, id: Id) {
        ui.memory_mut(|m| {
            if self.wants_keyboard && self.keyboard {
                m.request_focus(id);
                m.set_focus_lock_filter(
                    id,
                    egui::EventFilter {
                        tab: true,
                        horizontal_arrows: true,
                        vertical_arrows: true,
                        escape: true,
                    },
                );
            } else if m.has_focus(id) {
                m.surrender_focus(id);
            }
        });
    }

    fn take_textures(&mut self, ctx: &egui::Context, textures: &[wire::Texture]) {
        let side = max_texture_side(ctx);
        for t in textures {
            // The renderer must never see one it cannot hold.
            let Some((image, options)) = bridge::texture_from(t, side) else {
                continue;
            };
            match t.pos {
                None => {
                    if let Some(handle) = self.textures.get_mut(&t.id) {
                        handle.set(image, options);
                    } else if self.textures.len() < TEXTURES_MAX {
                        let name = format!("plugin:{}:{}", self.plugin, t.id);
                        self.textures.insert(t.id, ctx.load_texture(name, image, options));
                    }
                }
                Some([x, y]) => {
                    // A part that does not fit would take the renderer down.
                    if let Some(handle) = self.textures.get_mut(&t.id) {
                        let [w, h] = handle.size();
                        let (x, y) = (x as usize, y as usize);
                        if x.saturating_add(image.size[0]) <= w && y.saturating_add(image.size[1]) <= h {
                            handle.set_partial([x, y], image, options);
                        }
                    }
                }
            }
        }
    }

    /// Paints the frame and does what came with it. Returns the height used.
    pub fn paint(&mut self, ui: &mut egui::Ui, out: wire::ShowOutput) -> f32 {
        let ctx = ui.ctx().clone();
        let id = self.id();
        self.take_textures(&ctx, &out.textures);
        let base = out.base as usize;
        let screen = ctx.content_rect();
        let over = ctx.layer_painter(LayerId::new(Order::Foreground, id.with("paint")));
        for (n, primitive) in out.paint.into_iter().take(PAINT_MAX).enumerate() {
            let Some(primitive) = primitive.known() else {
                continue;
            };
            let painter = |clip: [f32; 4]| {
                let clip = bridge::rect_from(clip);
                if n < base {
                    ui.painter().with_clip_rect(clip.intersect(ui.clip_rect()))
                } else {
                    over.with_clip_rect(clip.intersect(screen))
                }
            };
            match primitive {
                wire::Primitive::Mesh(mesh) => {
                    let Some(texture) = self.textures.get(&mesh.texture) else {
                        continue;
                    };
                    if let Some(shape) = bridge::mesh_from(&mesh, texture.id()) {
                        painter(mesh.clip).add(egui::Shape::mesh(shape));
                    }
                }
                wire::Primitive::Frost(f) => {
                    let rect = bridge::rect_from(f.rect).intersect(screen);
                    if !rect.is_positive() {
                        continue;
                    }
                    let key = egui::Id::unique((id, f.id)).value();
                    let filter = frost::Filter {
                        blur: f.blur.clamp(0.0, 64.0),
                        saturate: f.saturate.clamp(0.0, 4.0),
                        brightness: f.brightness.clamp(0.0, 4.0),
                    };
                    let [nw, ne, sw, se] = f.radius;
                    let radius = egui::CornerRadius { nw, ne, sw, se };
                    painter(f.clip).add(if f.draw {
                        frost::draw_shape(rect, key, radius, filter)
                    } else {
                        frost::copy_shape(rect, key, filter)
                    });
                }
            }
        }
        for gone in out.free_textures {
            self.textures.remove(&gone);
        }

        let overlays: Vec<Rect> = out
            .overlays
            .into_iter()
            .take(OVERLAYS_MAX)
            .map(|r| bridge::rect_from(r).intersect(screen))
            .filter(Rect::is_positive)
            .collect();
        if overlays != self.overlays {
            self.overlays = overlays;
            ctx.request_repaint();
        }
        if self.over || self.captured {
            ctx.set_cursor_icon(bridge::cursor_from(&out.cursor));
        }
        if let Some(text) = out.copy.filter(|t| t.len() <= COPY_MAX) {
            ctx.copy_text(text);
        }
        // Only a click or Enter of the operator's opens a page.
        if let Some(url) = out.open_url {
            if self.acted && (url.starts_with("https://") || url.starts_with("http://")) {
                ctx.open_url(egui::OpenUrl::new_tab(url));
            }
        }
        if let Some([rect, cursor]) = out.ime {
            if self.keyboard {
                ctx.output_mut(|o| {
                    o.ime = Some(egui::output::IMEOutput {
                        purpose: egui::IMEPurpose::Normal,
                        rect: bridge::rect_from(rect),
                        cursor_rect: bridge::rect_from(cursor),
                        should_interrupt_composition: false,
                    });
                });
            }
        }
        if self.wants_keyboard != out.wants_keyboard {
            self.wants_keyboard = out.wants_keyboard;
            self.hold_focus(ui, id);
            ctx.request_repaint();
        }
        if out.used_wheel && self.over {
            ui.input_mut(|i| i.smooth_scroll_delta = egui::Vec2::ZERO);
        }
        if let Some(ms) = out.repaint_after_ms {
            ctx.request_repaint_after(std::time::Duration::from_millis(ms));
        }
        if out.height.is_finite() {
            out.height.clamp(0.0, 1.0e6)
        } else {
            0.0
        }
    }
}
