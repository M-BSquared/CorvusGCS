//! A plugin that is a WebAssembly module, as the registry sees one: the same
//! [`Plugin`] trait a part of the program implements. Each call becomes one call
//! into the module ([`corvus_api::wire`]): what changed around the plugin and
//! what arrived for it go in, what it asks for comes out and is done here, as
//! far as its [permissions](permissions) go.
//!
//! A module that traps (a panic, too much memory, a loop) is raised here as a
//! panic, which the registry turns into a stopped plugin.

pub mod permissions;
pub mod runtime;
#[cfg(test)]
pub mod testlib;
#[cfg(test)]
mod tests;
mod view;

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::io::Read;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use corvus_api::bridge;
use corvus_api::net::fetch::{self, Fetch};
use corvus_api::plugin::options::OptionSpec;
use corvus_api::plugin::ssh::SshNeed;
use corvus_api::plugin::{Host, Info, MapEvent, McpTool, Plugin, Reply};
use corvus_api::ui::{pagekit, pathfield, style};
use corvus_api::wire::{self, Delivery, Effect, Maybe, Op, Ret};
use corvus_package::Manifest;
use eframe::egui;
use serde_json::{json, Value};

use crate::net::http::{self, Server};
use crate::net::stream::Stream;
use permissions::Granted;
pub use runtime::{Instance, Sandbox};

/// The most requests of one plugin that are under way at once.
const REQUESTS_MAX: usize = 32;
/// The most deliveries waiting for a plugin; the oldest events go first.
const INBOX_MAX: usize = 2000;
const STREAMS_MAX: usize = 16;
const FETCHES_MAX: usize = 8;
const FETCH_WAIT_MAX: Duration = Duration::from_secs(1800);
const SHAPES_MAX: usize = 5000;
/// The most of an answer that is handed to a plugin.
const ANSWER_MAX: u64 = 32 * 1024 * 1024;
/// What a module assumes until the first frame says what the renderer takes.
const MAX_TEXTURE_SIDE: u32 = 2048;

#[derive(Default)]
struct Inbox {
    waiting: VecDeque<Delivery>,
}

impl Inbox {
    fn push(&mut self, d: Delivery) {
        if self.waiting.len() >= INBOX_MAX {
            // An answer is waited for; an event of a stream is one of many.
            if let Some(i) = self.waiting.iter().position(|d| matches!(d, Delivery::Event(_))) {
                self.waiting.remove(i);
            }
        }
        self.waiting.push_back(d);
    }
}

type Shared = Arc<Mutex<Inbox>>;

fn push(inbox: &Shared, d: Delivery) {
    inbox.lock().unwrap_or_else(|p| p.into_inner()).push(d);
}

fn failed(id: u64, why: String) -> Delivery {
    Delivery::Answer(wire::Answer {
        id,
        status: 0,
        body: Value::Null,
        error: Some(why),
    })
}

/// What the module was last told, so only what changed is told again.
#[derive(Default)]
struct Told {
    state: Option<Value>,
    settings: Option<Value>,
    options: Option<Value>,
    look: Option<(style::Palette, bool, bool, bool, i64)>,
    language: u64,
    live: Vec<String>,
    topics: HashMap<String, u64>,
    bus_sent: bool,
}

struct Inner {
    instance: Instance,
    /// Connections to other servers under way, under the module's numbers.
    fetches: HashMap<u64, Fetch>,
    granted: Granted,
    told: Told,
    inbox: Shared,
    in_flight: Arc<AtomicUsize>,
    /// Streams under the module's numbers; dropping one ends it.
    streams: HashMap<u64, Stream>,
    /// Saves on their way: the module's number and the answer.
    saves: Vec<(u64, Reply)>,
    /// Effects of a call that had no host to do them with.
    later: Vec<Effect>,
    view: view::View,
}

pub struct WasmPlugin {
    info: Info,
    options: Vec<OptionSpec>,
    inner: RefCell<Inner>,
}

fn leak(text: &str) -> &'static str {
    Box::leak(text.to_string().into_boxed_str())
}

impl WasmPlugin {
    /// Starts the module `bytes`, whose manifest the caller has read and
    /// found fit for this version. `cache` is the folder its compiled form is
    /// kept in.
    pub fn load(
        sandbox: &Arc<Sandbox>,
        manifest: &Manifest,
        bytes: &[u8],
        cache: Option<&Path>,
    ) -> Result<WasmPlugin, String> {
        let granted = Granted::new(&manifest.permissions);
        let files = granted.has(permissions::FILES);
        let mut instance = Instance::start(sandbox, &manifest.id, bytes, files, cache)?;
        let hello = instance.call(&wire::Call {
            now: now(),
            op: Some(Maybe::Known(Op::Init(wire::Init {
                protocol: wire::PROTOCOL,
                corvus: corvus_core::version::version().to_string(),
                max_texture_side: MAX_TEXTURE_SIDE,
            }))),
            ..wire::Call::default()
        })?;
        let Some(Ret::Hello(hello)) = hello.ret.and_then(Maybe::known) else {
            return Err("the module did not say who it is".into());
        };
        // The program shows the plugin's name, options and dialogs, so it translates them too.
        let lang: Vec<(&'static str, &'static str)> = hello
            .lang
            .iter()
            .take(64)
            .map(|(code, body)| (leak(code), leak(body)))
            .collect();
        corvus_api::i18n::register_plugin_files(Box::leak(lang.into_boxed_slice()));
        // The registry names a plugin by `&'static str`; a plugin is loaded once a run.
        let info = Info {
            id: leak(&manifest.id),
            name: leak(&manifest.name),
            icon: leak(&manifest.icon),
            description: leak(&manifest.description),
            version: leak(&manifest.version),
            order: manifest.order,
            can_tab: manifest.can_tab,
        };
        Ok(WasmPlugin {
            info,
            options: hello.options.into_iter().filter_map(bridge::option_from).collect(),
            inner: RefCell::new(Inner {
                instance,
                granted,
                told: Told::default(),
                inbox: Shared::default(),
                in_flight: Arc::default(),
                fetches: HashMap::new(),
                streams: HashMap::new(),
                saves: Vec::new(),
                later: Vec::new(),
                view: view::View::new(info.id),
            }),
        })
    }

    /// Where the view's popups are.
    #[cfg(test)]
    pub fn popups(&self) -> Vec<egui::Rect> {
        self.inner.borrow().view.overlays().to_vec()
    }

    #[cfg(test)]
    pub fn set_picker(&mut self, picker: runtime::Picker) {
        self.inner.get_mut().instance.set_picker(picker);
    }

    /// One call with a host: tells the module what changed, and does what it
    /// asks for.
    fn run(&mut self, op: Op, h: &mut Host) -> Ret {
        let id = self.info.id;
        self.inner.get_mut().call(id, op, h)
    }
}

fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

impl Told {
    fn look() -> (style::Palette, bool, bool, bool, i64) {
        (
            style::palette(),
            style::touch(),
            pagekit::link_up(),
            pathfield::backend_remote(),
            corvus_api::data::clock::local_offset(),
        )
    }

    fn env(&mut self, h: &Host) -> wire::Env {
        let mut env = wire::Env::default();
        if self.state.as_ref() != Some(h.state) {
            self.state = Some(h.state.clone());
            env.state = self.state.clone();
        }
        let settings = h.settings();
        if self.settings.as_ref() != Some(&settings) {
            self.settings = Some(settings.clone());
            env.settings = Some(settings);
        }
        if !matches!(&self.options, Some(Value::Object(o)) if o == h.options()) {
            self.options = Some(Value::Object(h.options().clone()));
            env.options = self.options.clone();
        }
        let look = Told::look();
        if self.look.as_ref() != Some(&look) {
            env.look = Some(wire::Look {
                palette: serde_json::to_value(look.0).unwrap_or(Value::Null),
                touch: look.1,
                link_up: look.2,
                backend_remote: look.3,
                utc_offset: look.4,
            });
            self.look = Some(look);
        }
        let language = corvus_api::i18n::revision();
        if self.language != language {
            self.language = language;
            let (code, strings) = corvus_api::i18n::table_in_force();
            env.language = Some(wire::Language { code, strings });
        }
        let bus = h.bus();
        let live = bus.live();
        let topics: Vec<(&str, u64, &Value)> = bus.topics().collect();
        let same = self.bus_sent
            && self.live == live
            && self.topics.len() == topics.len()
            && topics.iter().all(|(k, rev, _)| self.topics.get(*k) == Some(rev));
        if !same {
            env.bus = Some(wire::BusState {
                live: live.clone(),
                topics: topics
                    .iter()
                    .map(|(k, rev, v)| {
                        let new = self.topics.get(*k) != Some(rev);
                        (k.to_string(), *rev, new.then(|| (*v).clone()))
                    })
                    .collect(),
            });
            self.topics = topics.iter().map(|(k, rev, _)| (k.to_string(), *rev)).collect();
            self.live = live;
            self.bus_sent = true;
        }
        env
    }
}

impl Inner {
    fn deliveries(&mut self) -> Vec<Maybe<Delivery>> {
        let mut out: Vec<Maybe<Delivery>> = Vec::new();
        self.saves.retain(|(id, reply)| match reply.take() {
            None => true,
            Some(answer) => {
                let (status, body, error) = match answer {
                    Ok(body) => (200, body, None),
                    Err(e) => (0, Value::Null, Some(e)),
                };
                out.push(Maybe::Known(Delivery::Answer(wire::Answer {
                    id: *id,
                    status,
                    body,
                    error,
                })));
                false
            }
        });
        self.fetches.retain(|id, fetch| match fetch.take() {
            None => true,
            Some(answer) => {
                out.push(Maybe::Known(Delivery::Fetched(fetched(*id, answer))));
                false
            }
        });
        let mut inbox = self.inbox.lock().unwrap_or_else(|p| p.into_inner());
        out.extend(inbox.waiting.drain(..).map(Maybe::Known));
        out
    }

    /// One call into the module. A module that cannot go on panics here.
    fn exchange(&mut self, call: wire::Call) -> (Vec<Effect>, Ret) {
        match self.instance.call(&call) {
            Ok(reply) => (
                wire::known(reply.effects).collect(),
                reply
                    .ret
                    .and_then(Maybe::known)
                    .unwrap_or(Ret::Unsupported(wire::Empty {})),
            ),
            Err(why) => {
                self.streams.clear();
                self.fetches.clear();
                panic!("{why}");
            }
        }
    }

    fn call(&mut self, id: &str, op: Op, h: &mut Host) -> Ret {
        let call = wire::Call {
            env: self.told.env(h),
            deliveries: self.deliveries(),
            now: h.now,
            op: Some(Maybe::Known(op)),
        };
        let (effects, ret) = self.exchange(call);
        let mut all = std::mem::take(&mut self.later);
        all.extend(effects);
        for effect in all {
            self.apply(id, effect, h);
        }
        ret
    }

    /// A call from a place that has no host: what it asks for waits for the next one.
    fn call_alone(&mut self, op: Op) -> Ret {
        let call = wire::Call {
            deliveries: self.deliveries(),
            now: now(),
            op: Some(Maybe::Known(op)),
            ..wire::Call::default()
        };
        let (effects, ret) = self.exchange(call);
        self.later.extend(effects);
        ret
    }

    fn request(&mut self, ask: wire::Request, h: &Host) {
        let allowed = self
            .granted
            .route(&ask.method, &ask.path)
            .and_then(|()| self.granted.body(&ask.path, ask.body.as_ref()));
        if let Err(why) = allowed {
            eprintln!("{why}");
            push(&self.inbox, failed(ask.id, why));
            h.ctx.request_repaint();
            return;
        }
        if self.in_flight.fetch_add(1, Ordering::SeqCst) >= REQUESTS_MAX {
            self.in_flight.fetch_sub(1, Ordering::SeqCst);
            push(&self.inbox, failed(ask.id, "Too many requests at once".into()));
            h.ctx.request_repaint();
            return;
        }
        let (server, ctx) = (h.server.clone(), h.ctx.clone());
        let (inbox, in_flight) = (self.inbox.clone(), self.in_flight.clone());
        std::thread::spawn(move || {
            let answer = perform(&server, &ask);
            push(&inbox, answer);
            in_flight.fetch_sub(1, Ordering::SeqCst);
            ctx.request_repaint();
        });
    }

    /// Another server for a plugin that may connect to one, or the station's own `/mcp` for
    /// the assistant. The station is not reached by the first: a connection from this computer
    /// to the backend could read what no permission gave. No key goes with either.
    fn fetch(&mut self, ask: wire::Fetch, h: &Host) {
        match self.allowed_fetch(&ask, h.server) {
            Ok((url, station)) => {
                let request = fetch::Request {
                    method: ask.method,
                    url,
                    headers: ask.headers,
                    body: ask.body,
                    wait: Duration::from_millis(ask.timeout_ms).min(FETCH_WAIT_MAX),
                };
                let server = h.server.clone();
                let started = fetch::start_where(h.ctx, request, move |to| station || !is_station(&server, to));
                self.fetches.insert(ask.id, started);
            }
            Err(why) => {
                push(&self.inbox, Delivery::Fetched(fetched(ask.id, Err(why))));
                h.ctx.request_repaint();
            }
        }
    }

    /// The address to go to, and whether it is the station's.
    fn allowed_fetch(&self, ask: &wire::Fetch, server: &Server) -> Result<(String, bool), String> {
        if self.fetches.len() >= FETCHES_MAX {
            return Err("Too many connections at once".into());
        }
        if ask.url.starts_with('/') {
            if !self.granted.has(permissions::ASSISTANT) || ask.url != "/mcp" {
                return Err(format!("This plugin is not allowed to use {}", ask.url));
            }
            return Ok((format!("{}/mcp", server.url()), true));
        }
        if !self.granted.has(permissions::FETCH) {
            return Err("This plugin is not allowed to connect to other servers".into());
        }
        Ok((ask.url.clone(), false))
    }

    fn subscribe(&mut self, ask: wire::Subscribe, h: &Host) {
        let allowed = self.granted.topics(&ask.topics);
        if allowed.is_err() || self.streams.len() >= STREAMS_MAX {
            if let Err(why) = allowed {
                eprintln!("{why}");
            }
            push(
                &self.inbox,
                Delivery::Event(wire::BackendEvent {
                    id: ask.id,
                    topic: "error".into(),
                    data: Value::Null,
                }),
            );
            return;
        }
        let (inbox, id) = (self.inbox.clone(), ask.id);
        let stream = h.events(&ask.topics, move |topic, data| {
            push(
                &inbox,
                Delivery::Event(wire::BackendEvent {
                    id,
                    topic: topic.to_string(),
                    data: data.clone(),
                }),
            );
        });
        self.streams.insert(ask.id, stream);
    }

    fn apply(&mut self, id: &str, effect: Effect, h: &mut Host) {
        match effect {
            Effect::Notify(line) => h.notify(&line.level, &line.text),
            Effect::Console(line) => h.console(&line.text, &line.level),
            Effect::Terminal(t) => {
                if self.granted.has(permissions::SSH) {
                    h.terminal(t.session, t.reattach, t.existing_only);
                }
            }
            Effect::SshSetup(s) => {
                if self.granted.has(permissions::SSH) {
                    h.ssh_setup(
                        SshNeed {
                            name: s.name,
                            needs: s.needs,
                            error: s.error,
                        },
                        s.tag,
                    );
                }
            }
            Effect::Fit(fit) => h.fit(corvus_api::plugin::shapes::clean_coords(&fit.coords)),
            Effect::Pick(_) => h.pick_point(),
            Effect::CancelPick(_) => h.cancel_pick(),
            Effect::Shapes(shapes) => {
                let set = h.map();
                set.clear();
                for shape in wire::known(shapes.shapes).take(SHAPES_MAX) {
                    bridge::shape_into(set, shape);
                }
            }
            Effect::Request(ask) => self.request(ask, h),
            Effect::Save(save) => {
                let reply = h.save_settings(&save.patch, save.replace);
                // What the module saved is what it knows already.
                self.told.settings = Some(h.settings());
                self.saves.push((save.id, reply));
            }
            Effect::Subscribe(ask) => self.subscribe(ask, h),
            Effect::Unsubscribe(s) => {
                self.streams.remove(&s.id);
            }
            Effect::Fetch(ask) => self.fetch(ask, h),
            Effect::CancelFetch(f) => {
                self.fetches.remove(&f.id);
            }
            Effect::Publish(p) => {
                if !h.publish(&p.name, p.value) {
                    eprintln!("plugin {id}: {} is too large to publish", p.name);
                }
            }
            Effect::Retract(p) => h.retract(&p.name),
            Effect::Send(s) => {
                h.send(&s.to, s.msg);
            }
            Effect::Repaint(r) => h.ctx.request_repaint_after(Duration::from_millis(r.after_ms)),
        }
    }
}

fn fetched(id: u64, answer: Result<fetch::Answer, String>) -> wire::Fetched {
    match answer {
        Ok(a) => wire::Fetched {
            id,
            status: a.status,
            headers: a.headers,
            body: a.body,
            error: None,
        },
        Err(e) => wire::Fetched {
            id,
            error: Some(e),
            ..wire::Fetched::default()
        },
    }
}

/// Whether `to` is where this station's backend listens: its port on any address of this
/// computer.
fn is_station(server: &Server, to: std::net::SocketAddr) -> bool {
    let ip = to.ip();
    // Only an address of this computer can be bound.
    to.port() == server.port
        && server.is_loopback()
        && (ip.is_loopback() || ip.is_unspecified() || std::net::UdpSocket::bind((ip, 0)).is_ok())
}

/// Calls the route, which the permissions have let through.
fn perform(server: &Server, ask: &wire::Request) -> Delivery {
    let wait = Duration::from_secs(if ask.method == "GET" { 30 } else { 60 });
    let body = ask.body.as_ref().map(Value::to_string);
    let done = (|| -> Result<(u16, Value), String> {
        let r = http::request(
            server,
            &ask.method,
            &ask.path,
            body.as_deref(),
            "application/json",
            wait,
        )
        .map_err(|e| format!("{}{}: {e}", server.url(), ask.path))?;
        let status = r.status;
        let mut text = Vec::new();
        r.body
            .take(ANSWER_MAX + 1)
            .read_to_end(&mut text)
            .map_err(|e| format!("{}: {e}", ask.path))?;
        if text.len() as u64 > ANSWER_MAX {
            return Err(format!("The answer of {} is too large", ask.path));
        }
        let value = serde_json::from_slice(&text).map_err(|_| format!("Invalid response from {}", ask.path))?;
        Ok((status, value))
    })();
    match done {
        Ok((status, body)) => Delivery::Answer(wire::Answer {
            id: ask.id,
            status,
            body: permissions::answer(&ask.path, body),
            error: None,
        }),
        Err(e) => failed(ask.id, e),
    }
}

impl Plugin for WasmPlugin {
    fn info(&self) -> Info {
        self.info
    }

    fn options(&self) -> Vec<OptionSpec> {
        self.options.clone()
    }

    fn start(&mut self, h: &mut Host) {
        self.run(Op::Start(wire::Empty {}), h);
    }

    fn tick(&mut self, h: &mut Host) {
        self.run(Op::Tick(wire::Empty {}), h);
    }

    fn open(&mut self, h: &mut Host) {
        self.run(Op::Open(wire::Empty {}), h);
    }

    fn show(&mut self, ui: &mut egui::Ui, area: egui::Rect, h: &mut Host) -> f32 {
        let id = self.info.id;
        let inner = self.inner.get_mut();
        let input = inner.view.input(ui, area);
        let Ret::Show(out) = inner.call(id, Op::Show(Box::new(input)), h) else {
            return 0.0;
        };
        inner.view.paint(ui, *out)
    }

    fn close(&mut self, h: &mut Host) {
        self.run(Op::Close(wire::Empty {}), h);
        self.inner.get_mut().view.closed();
    }

    fn options_changed(&mut self, h: &mut Host) {
        self.run(Op::OptionsChanged(wire::Empty {}), h);
    }

    fn map_event(&mut self, e: MapEvent, h: &mut Host) {
        self.run(Op::MapEvent(bridge::map_event_to(&e)), h);
    }

    fn message(&mut self, from: &str, msg: &Value, h: &mut Host) {
        self.run(
            Op::Message(wire::Message {
                from: from.to_string(),
                msg: msg.clone(),
            }),
            h,
        );
    }

    fn ssh_setup_done(&mut self, tag: u64, saved: bool, h: &mut Host) {
        self.run(Op::SshSetupDone(wire::SshSetupDone { tag, saved }), h);
    }

    fn mcp_tools(&self, h: &Host) -> Vec<McpTool> {
        let mut inner = self.inner.borrow_mut();
        if !inner.granted.has(permissions::AI_TOOLS) {
            return Vec::new();
        }
        let call = wire::Call {
            env: inner.told.env(h),
            deliveries: inner.deliveries(),
            now: h.now,
            op: Some(Maybe::Known(Op::McpTools(wire::Empty {}))),
        };
        let (effects, ret) = inner.exchange(call);
        inner.later.extend(effects);
        match ret {
            Ret::McpTools(t) => t.tools.into_iter().map(bridge::tool_from).collect(),
            _ => Vec::new(),
        }
    }

    fn mcp_call(&mut self, name: &str, args: &Value, token: u64, h: &mut Host) {
        self.run(
            Op::McpCall(wire::McpCall {
                name: name.to_string(),
                args: args.clone(),
                token,
            }),
            h,
        );
    }

    fn mcp_take(&mut self, token: u64) -> Option<Result<Value, String>> {
        match self.inner.get_mut().call_alone(Op::McpTake(wire::McpTake { token })) {
            Ret::McpTaken(t) if !t.ready => None,
            Ret::McpTaken(t) => Some(match t.error {
                Some(e) => Err(e),
                None => Ok(t.ok.unwrap_or_else(|| json!(null))),
            }),
            _ => Some(Err("The plugin offers no such tool right now.".into())),
        }
    }
}
