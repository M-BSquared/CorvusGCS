//! What the program and a plugin (a WebAssembly module) say to each other.
//!
//! The program calls the plugin with one [`Call`] and gets one [`Reply`] back,
//! both as MessagePack with field names. A plugin may be older or newer than the
//! program it runs in: it names the oldest Corvus GCS it needs, and every one
//! from there on has to take it. So **this file only ever grows**:
//!
//! * A new field is `#[serde(default)]` (every struct here is as a whole). A
//!   field is never renamed, removed or given another type.
//! * A new operation, effect, delivery or input event is a new variant at the
//!   end. Whoever does not know it skips it ([`Maybe`]).
//! * What a variant carries is a struct, so it can grow too.
//!
//! Nothing here is a type of egui: its own serialisation changes with its
//! versions, and a plugin carries its own copy of egui.

use serde::{Deserialize, Serialize};
use serde_bytes::ByteBuf;
use serde_json::Value;

/// The version of the protocol. It says what a side can do and refuses nothing.
pub const PROTOCOL: u32 = 1;

/// The names the module exports and imports.
pub const EXPORT_ALLOC: &str = "corvus_alloc";
pub const EXPORT_CALL: &str = "corvus_call";
pub const EXPORT_MEMORY: &str = "memory";
pub const IMPORT_MODULE: &str = "corvus";
pub const IMPORT_HOST: &str = "host";

/// The custom section of a module that holds its manifest (`plugin.json`).
pub const MANIFEST_SECTION: &str = "corvus.manifest";

/// An item of a list that may be of a kind this side does not know yet.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Maybe<T> {
    Known(T),
    Unknown(Skipped),
}

/// What stood where something unknown was.
#[derive(Debug, Clone, PartialEq)]
pub struct Skipped;

impl Serialize for Skipped {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_unit()
    }
}

impl<'de> Deserialize<'de> for Skipped {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Skipped, D::Error> {
        serde::de::IgnoredAny::deserialize(deserializer).map(|_| Skipped)
    }
}

impl<T> Maybe<T> {
    pub fn known(self) -> Option<T> {
        match self {
            Maybe::Known(t) => Some(t),
            Maybe::Unknown(_) => None,
        }
    }
}

/// The items of `list` this side knows.
pub fn known<T>(list: Vec<Maybe<T>>) -> impl Iterator<Item = T> {
    list.into_iter().filter_map(Maybe::known)
}

pub fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, String> {
    rmp_serde::to_vec_named(value).map_err(|e| e.to_string())
}

pub fn decode<'a, T: Deserialize<'a>>(bytes: &'a [u8]) -> Result<T, String> {
    rmp_serde::from_slice(bytes).map_err(|e| e.to_string())
}

// ---- program to plugin --------------------------------------------------

/// One call into a plugin.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Call {
    /// What changed in the plugin's surroundings since the last call.
    pub env: Env,
    /// Answers and events that arrived, to hand out before `op` runs.
    pub deliveries: Vec<Maybe<Delivery>>,
    /// Seconds since 1970.
    pub now: f64,
    pub op: Option<Maybe<Op>>,
}

/// What the plugin may look at without asking. Only what changed is sent.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Env {
    /// The telemetry state.
    pub state: Option<Value>,
    /// The plugin's own saved settings, without its options.
    pub settings: Option<Value>,
    /// Every declared option, as set or by default.
    pub options: Option<Value>,
    pub look: Option<Look>,
    pub language: Option<Language>,
    pub bus: Option<BusState>,
}

/// The theme and what else every widget reads.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Look {
    /// The palette's tokens by name; one the program does not send keeps the
    /// plugin's own.
    pub palette: Value,
    pub touch: bool,
    pub link_up: bool,
    pub backend_remote: bool,
    /// Seconds east of UTC on the operator's computer.
    pub utc_offset: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Language {
    pub code: String,
    /// The program's own table of that language and the operator's file; the
    /// plugin lays its own entries underneath.
    pub strings: Vec<(String, String)>,
}

/// What the other plugins offer.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct BusState {
    /// The plugins that are running.
    pub live: Vec<String>,
    /// `(topic, revision, value)`: the value only when the revision is new to
    /// this plugin. A topic that is not listed is gone.
    pub topics: Vec<(String, u64, Option<Value>)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Op {
    /// The first call: answered with [`Ret::Hello`].
    Init(Init),
    Start(Empty),
    Tick(Empty),
    Open(Empty),
    Close(Empty),
    OptionsChanged(Empty),
    Show(Box<ShowInput>),
    MapEvent(MapEvent),
    Message(Message),
    SshSetupDone(SshSetupDone),
    McpTools(Empty),
    McpCall(McpCall),
    McpTake(McpTake),
}

/// For a variant that carries nothing yet.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Empty {}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Init {
    pub protocol: u32,
    /// The version of the program, for a plugin that does more on a newer one.
    pub corvus: String,
    pub max_texture_side: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct MapEvent {
    /// "click", "drag", "drag_end", "picked" or "pick_cancelled".
    pub kind: String,
    pub key: String,
    pub at: [f64; 2],
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Message {
    pub from: String,
    pub msg: Value,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SshSetupDone {
    pub tag: u64,
    pub saved: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct McpCall {
    pub name: String,
    pub args: Value,
    pub token: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct McpTake {
    pub token: u64,
}

/// One frame of the plugin's view. Rectangles are `[left, top, right, bottom]`
/// in points of the program's window.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ShowInput {
    /// The whole window: popups and dialogs of the plugin may use all of it.
    pub screen: [f32; 4],
    /// Where the view goes (`Plugin::show`).
    pub area: [f32; 4],
    /// What of it can be seen.
    pub clip: [f32; 4],
    pub pixels_per_point: f32,
    pub time: f64,
    pub predicted_dt: f32,
    pub focused: bool,
    pub modifiers: Modifiers,
    pub events: Vec<Maybe<Event>>,
    /// Ask again for every texture: the program lost them.
    pub textures_lost: bool,
    /// The widest texture the program's renderer takes, 0 when it did not say.
    pub max_texture_side: u32,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Modifiers {
    pub alt: bool,
    pub ctrl: bool,
    pub shift: bool,
    pub mac_cmd: bool,
    pub command: bool,
}

/// What the operator did. Keys go by the names `egui::Key::name` gives.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Event {
    PointerMoved {
        x: f32,
        y: f32,
    },
    PointerButton {
        x: f32,
        y: f32,
        /// 0 primary, 1 secondary, 2 middle, 3 and 4 the side buttons.
        button: u8,
        pressed: bool,
        modifiers: Modifiers,
    },
    PointerGone {},
    Wheel {
        /// 0 points, 1 lines, 2 pages.
        unit: u8,
        x: f32,
        y: f32,
        /// 0 start, 1 move, 2 end, 3 cancel.
        phase: u8,
        modifiers: Modifiers,
    },
    Zoom {
        factor: f32,
    },
    Key {
        key: String,
        physical: Option<String>,
        pressed: bool,
        repeat: bool,
        modifiers: Modifiers,
    },
    Text {
        text: String,
    },
    Paste {
        text: String,
    },
    Copy {},
    Cut {},
    ImeEnabled {},
    ImePreedit {
        text: String,
    },
    ImeCommit {
        text: String,
    },
    ImeDisabled {},
    Touch {
        device: u64,
        id: u64,
        /// As `Wheel::phase`.
        phase: u8,
        x: f32,
        y: f32,
        force: Option<f32>,
    },
    WindowFocused {
        focused: bool,
    },
}

/// Something the plugin asked for, arriving.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Delivery {
    /// The answer of an [`Effect::Request`] or [`Effect::Save`].
    Answer(Answer),
    /// An event of an [`Effect::Subscribe`].
    Event(BackendEvent),
    /// The answer of an [`Effect::Fetch`].
    Fetched(Fetched),
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Answer {
    pub id: u64,
    /// The HTTP status, or 0 when there was none.
    pub status: u16,
    pub body: Value,
    /// Why there is no body.
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct BackendEvent {
    pub id: u64,
    /// The topic, or "error" when the connection dropped.
    pub topic: String,
    pub data: Value,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Fetched {
    pub id: u64,
    pub status: u16,
    /// Names in lower case.
    pub headers: Vec<(String, String)>,
    pub body: String,
    pub error: Option<String>,
}

// ---- plugin to program --------------------------------------------------

/// What a call came to.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Reply {
    pub effects: Vec<Maybe<Effect>>,
    pub ret: Option<Maybe<Ret>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Ret {
    Done(Empty),
    /// The plugin does not know the operation: it is older than the program.
    Unsupported(Empty),
    Hello(Hello),
    Show(Box<ShowOutput>),
    McpTools(McpTools),
    McpTaken(McpTaken),
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Hello {
    pub protocol: u32,
    pub options: Vec<OptionSpec>,
    /// The plugin's language files, `(code, file text)`: the program translates the
    /// plugin's name, options and dialogs with them, and hands the table back.
    pub lang: Vec<(String, String)>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct OptionSpec {
    pub key: String,
    pub label: String,
    pub hint: String,
    /// "toggle", "select", "text" or "number".
    pub kind: String,
    pub choices: Vec<(String, String)>,
    pub placeholder: String,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub step: Option<f64>,
    pub default: Value,
    pub secret: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct McpTools {
    pub tools: Vec<McpTool>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct McpTool {
    pub name: String,
    pub title: String,
    pub description: String,
    pub input_schema: Value,
    pub confirm: bool,
    pub read_only: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct McpTaken {
    /// False while the call is still under way.
    pub ready: bool,
    pub ok: Option<Value>,
    pub error: Option<String>,
}

/// One frame the plugin drew.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ShowOutput {
    /// The height the view used.
    pub height: f32,
    pub paint: Vec<Maybe<Primitive>>,
    /// The first `base` of `paint` are the view itself; the rest lies over the
    /// whole window (popups, menus, tooltips, dialogs).
    pub base: u32,
    /// Where those are, so that nothing under them takes the pointer.
    pub overlays: Vec<[f32; 4]>,
    pub textures: Vec<Texture>,
    pub free_textures: Vec<u64>,
    /// The CSS name of the pointer's shape.
    pub cursor: String,
    pub copy: Option<String>,
    pub open_url: Option<String>,
    /// A text field has the keyboard: the program's own keys stay quiet.
    pub wants_keyboard: bool,
    /// `[rect, cursor rect]` for an input method.
    pub ime: Option<[[f32; 4]; 2]>,
    /// The plugin scrolled something with the wheel, so the view does not.
    pub used_wheel: bool,
    /// Draw again after this long, 0 for the next frame.
    pub repaint_after_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Primitive {
    Mesh(Mesh),
    Frost(Frost),
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Mesh {
    pub clip: [f32; 4],
    pub texture: u64,
    /// [`VERTEX_BYTES`] each, little endian: x, y, u, v as `f32`, then the
    /// colour as premultiplied sRGBA.
    pub vertices: ByteBuf,
    /// `u32`, little endian, three to a triangle.
    pub indices: ByteBuf,
}

pub const VERTEX_BYTES: usize = 20;

/// The frosted glass, which the program's renderer draws.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Frost {
    pub clip: [f32; 4],
    pub rect: [f32; 4],
    pub id: u64,
    /// False copies what lies underneath, true draws it back.
    pub draw: bool,
    /// North west, north east, south west, south east.
    pub radius: [u8; 4],
    pub blur: f32,
    pub saturate: f32,
    pub brightness: f32,
}

/// A new texture, or a part of one.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Texture {
    pub id: u64,
    /// Where the part goes; `None` replaces the whole.
    pub pos: Option<[u32; 2]>,
    pub size: [u32; 2],
    /// Premultiplied sRGBA, row by row.
    pub rgba: ByteBuf,
    /// Linear filtering, else the nearest pixel.
    pub linear_magnify: bool,
    pub linear_minify: bool,
    /// Repeat outside the edges, else the edge pixel.
    pub repeat: bool,
}

/// Something the plugin asks the program to do. What needs a permission is
/// refused by the program, whatever the plugin says.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Effect {
    Notify(Line),
    Console(Line),
    Terminal(Terminal),
    SshSetup(SshSetup),
    Fit(Fit),
    Pick(Empty),
    CancelPick(Empty),
    /// All of the plugin's shapes on the map, whenever one changed.
    Shapes(Shapes),
    /// A call of a backend route.
    Request(Request),
    /// Saves the plugin's own settings.
    Save(Save),
    /// Follows push topics of the backend, comma separated.
    Subscribe(Subscribe),
    Unsubscribe(Id),
    /// A request to another server.
    Fetch(Fetch),
    CancelFetch(Id),
    Publish(Publish),
    Retract(Publish),
    Send(SendTo),
    /// Draw a frame again soon, after this many milliseconds.
    Repaint(Repaint),
    /// Shows a picture or video file in a card of the program.
    ShowFile(ShowFile),
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Line {
    pub level: String,
    pub text: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Terminal {
    pub session: Value,
    pub reattach: bool,
    pub existing_only: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ShowFile {
    pub path: String,
    pub title: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SshSetup {
    pub tag: u64,
    pub name: String,
    pub needs: String,
    pub error: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Fit {
    pub coords: Vec<[f64; 2]>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Shapes {
    pub shapes: Vec<Maybe<Shape>>,
}

/// One shape on the map, in longitude and latitude.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Shape {
    pub key: String,
    /// "line", "polygon", "text", "button" or "marker".
    pub kind: String,
    pub points: Vec<[f64; 2]>,
    /// Premultiplied sRGBA.
    pub color: [u8; 4],
    pub width: f32,
    pub opacity: f32,
    pub visible: bool,
    pub dashed: bool,
    pub fill_opacity: f32,
    pub outline: f32,
    pub text: String,
    pub title: String,
    pub size: f32,
    pub dot: bool,
    pub disabled: bool,
    pub icon: Vec<String>,
    pub selected: bool,
    pub draggable: bool,
    pub clickable: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Request {
    pub id: u64,
    /// "GET" or "POST".
    pub method: String,
    /// With its query, from `/api/`.
    pub path: String,
    pub body: Option<Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Save {
    pub id: u64,
    pub patch: Value,
    /// The patch is the whole, not merged into what is there.
    pub replace: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Subscribe {
    pub id: u64,
    pub topics: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Id {
    pub id: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Fetch {
    pub id: u64,
    /// `http://` or `https://`, or a path of this station beginning with `/`.
    pub url: String,
    pub method: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Publish {
    pub name: String,
    pub value: Value,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SendTo {
    pub to: String,
    pub msg: Value,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Repaint {
    pub after_ms: u64,
}

// ---- what the plugin asks the program during a call ---------------------

/// A question the plugin needs answered before it can go on (the import
/// [`IMPORT_HOST`]).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Query {
    /// The system's file dialog.
    PickPath(PickPath),
    /// The bytes of a file the operator picked for this plugin.
    ReadPicked(ReadPicked),
    /// A panic's words, before the module traps.
    Panic(Line),
    /// What goes to the program's log.
    Log(Line),
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PickPath {
    /// "file", "folder" or "save".
    pub mode: String,
    pub title: String,
    /// The suggested name of a file to write.
    pub name: String,
    /// The label of the kind of file, and its extensions without dots.
    pub filter: Option<(String, Vec<String>)>,
    pub start: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ReadPicked {
    pub path: String,
    /// The most bytes wanted.
    pub max: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Told {
    pub path: Option<String>,
    pub bytes: Option<ByteBuf>,
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A later side's call: a field, an operation, a delivery and an input
    /// event this one has never heard of.
    #[derive(Serialize)]
    struct LaterCall {
        env: Env,
        deliveries: Vec<LaterDelivery>,
        now: f64,
        op: LaterOp,
        tomorrow: u32,
    }

    #[derive(Serialize)]
    enum LaterDelivery {
        Hologram { id: u64 },
        Answer(Answer),
    }

    #[derive(Serialize)]
    enum LaterOp {
        Dance { steps: u32 },
    }

    #[test]
    fn what_a_later_side_sends_is_read_and_the_unknown_skipped() {
        let bytes = encode(&LaterCall {
            env: Env::default(),
            deliveries: vec![
                LaterDelivery::Hologram { id: 1 },
                LaterDelivery::Answer(Answer {
                    id: 2,
                    status: 200,
                    ..Answer::default()
                }),
            ],
            now: 5.0,
            op: LaterOp::Dance { steps: 3 },
            tomorrow: 7,
        })
        .unwrap();
        let call: Call = decode(&bytes).unwrap();
        assert_eq!(call.now, 5.0);
        let got: Vec<Delivery> = known(call.deliveries).collect();
        assert!(matches!(&got[..], [Delivery::Answer(a)] if a.id == 2));
        assert!(call.op.unwrap().known().is_none());
    }

    #[test]
    fn what_an_earlier_side_leaves_out_has_its_default() {
        #[derive(Serialize)]
        struct Early {
            area: [f32; 4],
        }
        let bytes = encode(&Early {
            area: [1.0, 2.0, 3.0, 4.0],
        })
        .unwrap();
        let got: ShowInput = decode(&bytes).unwrap();
        assert_eq!(got.area, [1.0, 2.0, 3.0, 4.0]);
        assert!(got.events.is_empty() && !got.textures_lost);
    }

    #[test]
    fn a_whole_frame_goes_both_ways() {
        let out = Reply {
            effects: vec![Maybe::Known(Effect::Pick(Empty {}))],
            ret: Some(Maybe::Known(Ret::Show(Box::new(ShowOutput {
                height: 12.0,
                paint: vec![Maybe::Known(Primitive::Mesh(Mesh {
                    clip: [0.0, 0.0, 1.0, 1.0],
                    texture: 3,
                    vertices: ByteBuf::from(vec![1; VERTEX_BYTES]),
                    indices: ByteBuf::from(vec![0; 12]),
                }))],
                ..ShowOutput::default()
            })))),
        };
        let back: Reply = decode(&encode(&out).unwrap()).unwrap();
        let Some(Ret::Show(show)) = back.ret.and_then(Maybe::known) else {
            panic!("not a frame");
        };
        assert_eq!(show.height, 12.0);
        let Some(Primitive::Mesh(m)) = known(show.paint).next() else {
            panic!("not a mesh");
        };
        assert_eq!((m.texture, m.vertices.len()), (3, VERTEX_BYTES));
        assert_eq!(back.effects.len(), 1);
    }
}
