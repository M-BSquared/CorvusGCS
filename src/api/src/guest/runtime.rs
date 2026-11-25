//! One plugin and everything it is handed for a call.

use serde_json::{json, Map, Value};

use egui::{self, LayerId, Order};

use crate::bridge;
use crate::net::http::Server;
use crate::plugin::bus::Bus;
use crate::plugin::options;
use crate::plugin::shapes::ShapeSet;
use crate::plugin::{Host, Info, Outbox, Plugin};
use crate::ui::{fonts, pagekit, pathfield, style};
use crate::wire::{self, Effect, Maybe, Op, Ret};

/// What a [`Host`] borrows.
struct Surroundings {
    info: Info,
    ctx: egui::Context,
    server: Server,
    state: Value,
    saved: Value,
    options: Map<String, Value>,
    shapes: ShapeSet,
    out: Outbox,
    bus: Bus,
    now: f64,
}

impl Surroundings {
    fn host(&mut self) -> Host<'_> {
        let mut host = Host::new(
            &self.ctx,
            &self.server,
            &self.state,
            self.info,
            &mut self.shapes,
            &mut self.saved,
            &self.options,
            &mut self.out,
            &mut self.bus,
        );
        host.now = self.now;
        host
    }
}

pub(super) struct Runtime {
    plugin: Box<dyn Plugin>,
    lang: &'static [(&'static str, &'static str)],
    s: Surroundings,
    max_texture_side: u32,
}

fn new_context() -> egui::Context {
    let ctx = egui::Context::default();
    egui_extras::install_image_loaders(&ctx);
    fonts::install(&ctx);
    style::apply(&ctx);
    ctx
}

impl Runtime {
    pub(super) fn new(plugin: Box<dyn Plugin>, lang: &'static [(&'static str, &'static str)]) -> Runtime {
        crate::i18n::register_plugin_files(lang);
        let info = plugin.info();
        Runtime {
            s: Surroundings {
                info,
                ctx: new_context(),
                server: Server::local(0),
                state: json!({}),
                saved: json!({}),
                options: Map::new(),
                shapes: ShapeSet::new(info.id),
                out: Outbox::default(),
                bus: Bus::default(),
                now: 0.0,
            },
            plugin,
            lang,
            max_texture_side: 2048,
        }
    }

    fn take_env(&mut self, env: wire::Env) {
        if let Some(state) = env.state {
            self.s.state = state;
        }
        if let Some(settings) = env.settings {
            self.s.saved = settings;
        }
        if let Some(Value::Object(options)) = env.options {
            self.s.options = options;
        }
        if let Some(look) = env.look {
            // A token the program does not send keeps this side's own.
            if let Ok(palette) = serde_json::from_value::<style::Palette>(look.palette) {
                style::set_palette(palette);
            }
            style::set_touch(look.touch);
            pagekit::set_link_up(look.link_up);
            pathfield::set_backend_remote(look.backend_remote);
            crate::data::clock::OFFSET.store(look.utc_offset, std::sync::atomic::Ordering::Relaxed);
            style::apply(&self.s.ctx);
        }
        if let Some(language) = env.language {
            crate::i18n::adopt(&language.code, language.strings);
        }
        if let Some(bus) = env.bus {
            self.s.bus.mirror(&bus.live, bus.topics);
        }
    }

    pub(super) fn handle(&mut self, call: wire::Call) -> wire::Reply {
        self.s.now = call.now;
        self.take_env(call.env);
        for delivery in wire::known(call.deliveries) {
            super::mail::deliver(delivery);
        }
        let ret = match call.op.and_then(Maybe::known) {
            Some(op) => self.run(op),
            None => Ret::Unsupported(wire::Empty {}),
        };
        self.flush();
        wire::Reply {
            effects: super::mail::take().into_iter().map(Maybe::Known).collect(),
            ret: Some(Maybe::Known(ret)),
        }
    }

    fn run(&mut self, op: Op) -> Ret {
        let done = Ret::Done(wire::Empty {});
        let Runtime { plugin, s, .. } = self;
        match op {
            Op::Init(init) => {
                self.max_texture_side = init.max_texture_side.max(256);
                return Ret::Hello(wire::Hello {
                    protocol: wire::PROTOCOL,
                    options: options::clean(self.plugin.options())
                        .into_iter()
                        .map(bridge::option_to)
                        .collect(),
                    lang: self.lang.iter().map(|(c, b)| (c.to_string(), b.to_string())).collect(),
                });
            }
            Op::Start(_) => plugin.start(&mut s.host()),
            Op::Tick(_) => plugin.tick(&mut s.host()),
            Op::Open(_) => plugin.open(&mut s.host()),
            Op::Close(_) => plugin.close(&mut s.host()),
            Op::OptionsChanged(_) => plugin.options_changed(&mut s.host()),
            Op::Show(input) => return Ret::Show(Box::new(self.show(*input))),
            Op::MapEvent(e) => match bridge::map_event_from(e) {
                Some(e) => plugin.map_event(e, &mut s.host()),
                None => return Ret::Unsupported(wire::Empty {}),
            },
            Op::Message(m) => plugin.message(&m.from, &m.msg, &mut s.host()),
            Op::SshSetupDone(d) => plugin.ssh_setup_done(d.tag, d.saved, &mut s.host()),
            Op::McpTools(_) => {
                let tools = plugin.mcp_tools(&s.host());
                return Ret::McpTools(wire::McpTools {
                    tools: tools.into_iter().map(bridge::tool_to).collect(),
                });
            }
            Op::McpCall(c) => plugin.mcp_call(&c.name, &c.args, c.token, &mut s.host()),
            Op::McpTake(t) => {
                return Ret::McpTaken(match plugin.mcp_take(t.token) {
                    None => wire::McpTaken::default(),
                    Some(Ok(v)) => wire::McpTaken {
                        ready: true,
                        ok: Some(v),
                        error: None,
                    },
                    Some(Err(e)) => wire::McpTaken {
                        ready: true,
                        ok: None,
                        error: Some(e),
                    },
                })
            }
        }
        done
    }

    /// What the plugin left in its [`Outbox`] and on its map, as effects.
    fn flush(&mut self) {
        let out = std::mem::take(&mut self.s.out);
        for (_, level, text) in out.notices {
            super::mail::push(Effect::Notify(wire::Line {
                level: level.into(),
                text,
            }));
        }
        for (level, text) in out.console {
            super::mail::push(Effect::Console(wire::Line { level, text }));
        }
        for (session, reattach, existing_only) in out.terminals {
            super::mail::push(Effect::Terminal(wire::Terminal {
                session,
                reattach,
                existing_only,
            }));
        }
        for (_, tag, need) in out.ssh_setups {
            super::mail::push(Effect::SshSetup(wire::SshSetup {
                tag,
                name: need.name,
                needs: need.needs,
                error: need.error,
            }));
        }
        if let Some(coords) = out.fit {
            super::mail::push(Effect::Fit(wire::Fit { coords }));
        }
        if out.cancel_pick {
            super::mail::push(Effect::CancelPick(wire::Empty {}));
        }
        if out.pick.is_some() {
            super::mail::push(Effect::Pick(wire::Empty {}));
        }
        if self.s.shapes.take_changed() {
            let shapes = self
                .s
                .shapes
                .shapes()
                .iter()
                .map(|s| Maybe::Known(bridge::shape_to(s)))
                .collect();
            super::mail::push(Effect::Shapes(wire::Shapes { shapes }));
        }
    }

    /// One frame of the view in this side's egui, as triangles.
    fn show(&mut self, input: wire::ShowInput) -> wire::ShowOutput {
        if input.textures_lost {
            self.s.ctx = new_context();
        }
        let screen = bridge::rect_from(input.screen);
        let area = bridge::rect_from(input.area);
        let clip = bridge::rect_from(input.clip);
        let ppp = if input.pixels_per_point.is_finite() && input.pixels_per_point > 0.0 {
            input.pixels_per_point
        } else {
            1.0
        };
        let mut events = vec![egui::Event::WindowFocused(input.focused)];
        events.extend(wire::known(input.events).filter_map(bridge::event_from));
        let mut raw = egui::RawInput {
            screen_rect: Some(screen),
            time: Some(input.time),
            predicted_dt: input.predicted_dt,
            focused: input.focused,
            max_texture_side: Some(match input.max_texture_side {
                0 => self.max_texture_side,
                side => side,
            } as usize),
            events,
            ..egui::RawInput::default()
        };
        raw.viewports
            .entry(raw.viewport_id)
            .or_default()
            .native_pixels_per_point = Some(ppp);
        raw.events.insert(
            0,
            egui::Event::ModifiersChanged(bridge::modifiers_from(input.modifiers)),
        );

        let ctx = self.s.ctx.clone();
        let (mut height, mut base, mut used_wheel) = (0.0f32, 0usize, false);
        let Runtime { plugin, s, .. } = self;
        let mut full = ctx.run_ui(raw, |root| {
            let wheel = root.input(|i| i.smooth_scroll_delta());
            let mut view = root.new_child(egui::UiBuilder::new().id_salt("view").max_rect(area));
            view.set_clip_rect(clip);
            height = plugin.show(&mut view, area, &mut s.host());
            let left = root.input(|i| i.smooth_scroll_delta());
            used_wheel = wheel != egui::Vec2::ZERO && left == egui::Vec2::ZERO;
            base = root
                .ctx()
                .graphics(|g| g.get(LayerId::background()).map_or(0, |l| l.all_entries().len()));
        });

        let mut shapes = std::mem::take(&mut full.shapes);
        let over = shapes.split_off(base.min(shapes.len()));
        let mut paint = Vec::new();
        for clipped in ctx.tessellate(shapes, full.pixels_per_point) {
            primitive(&mut paint, clipped);
        }
        let base = paint.len() as u32;
        for clipped in ctx.tessellate(over, full.pixels_per_point) {
            primitive(&mut paint, clipped);
        }

        // Popups, menus and dialogs take the pointer away from what is under
        // them in the program's window too; a tooltip does not.
        let overlays = ctx.memory(|m| {
            m.areas()
                .visible_layer_ids()
                .into_iter()
                .filter(|l| matches!(l.order, Order::Middle | Order::Foreground))
                .filter_map(|l| m.area_rect(l.id))
                .map(bridge::rect_to)
                .collect()
        });

        let mut deltas = std::mem::take(&mut full.textures_delta);
        let mut textures = Vec::new();
        for (id, list) in std::mem::take(&mut deltas.set) {
            if let egui::TextureId::Managed(id) = id {
                textures.extend(list.iter().map(|d| bridge::texture_to(id, d)));
            }
        }
        let free_textures = std::mem::take(&mut deltas.free)
            .into_iter()
            .filter_map(|id| match id {
                egui::TextureId::Managed(id) => Some(id),
                egui::TextureId::User(_) => None,
            })
            .collect();

        let platform = std::mem::take(&mut full.platform_output);
        let (mut copy, mut open_url) = (None, None);
        for command in platform.commands {
            match command {
                egui::OutputCommand::CopyText(text) => copy = Some(text),
                egui::OutputCommand::OpenUrl(open) => open_url = Some(open.url),
                _ => {}
            }
        }
        let repaint_after_ms = full
            .viewport_output
            .values()
            .map(|v| v.repaint_delay)
            .min()
            .filter(|d| d.as_secs() < 3600)
            .map(|d| d.as_millis() as u64);
        wire::ShowOutput {
            height: if height.is_finite() { height } else { 0.0 },
            paint,
            base,
            overlays,
            textures,
            free_textures,
            cursor: bridge::cursor_name(platform.cursor_icon).into(),
            copy,
            open_url,
            wants_keyboard: ctx.egui_wants_keyboard_input(),
            ime: platform
                .ime
                .map(|i| [bridge::rect_to(i.rect), bridge::rect_to(i.cursor_rect)]),
            used_wheel,
            repaint_after_ms,
        }
    }
}

fn primitive(into: &mut Vec<Maybe<wire::Primitive>>, clipped: egui::ClippedPrimitive) {
    let clip = clipped.clip_rect;
    match clipped.primitive {
        egui::epaint::Primitive::Mesh(mesh) => {
            let egui::TextureId::Managed(texture) = mesh.texture_id else {
                return;
            };
            if !mesh.indices.is_empty() {
                into.push(Maybe::Known(wire::Primitive::Mesh(bridge::mesh_to(
                    &mesh, clip, texture,
                ))));
            }
        }
        egui::epaint::Primitive::Callback(callback) => {
            let Some(marker) = callback.callback.downcast_ref::<crate::ui::frost::Marker>() else {
                return;
            };
            into.push(Maybe::Known(wire::Primitive::Frost(wire::Frost {
                clip: bridge::rect_to(clip),
                rect: bridge::rect_to(callback.rect),
                id: marker.id,
                draw: marker.draw,
                radius: [marker.radius.nw, marker.radius.ne, marker.radius.sw, marker.radius.se],
                blur: marker.filter.blur,
                saturate: marker.filter.saturate,
                brightness: marker.filter.brightness,
            })));
        }
    }
}
