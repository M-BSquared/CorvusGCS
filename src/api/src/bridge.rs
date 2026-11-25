//! Between the types of [`crate::wire`] and those each side works with. The
//! program and a plugin both use this file, each with its own copy of egui.

use egui::{self, epaint, Color32, CursorIcon, Pos2, Rect};
use serde_bytes::ByteBuf;

use crate::plugin::options::{Kind, OptionSpec};
use crate::plugin::shapes::{self, Shape};
use crate::plugin::{FolderScan, MapEvent, McpTool, Placed};
use crate::wire;

pub fn rect_to(r: Rect) -> [f32; 4] {
    [r.min.x, r.min.y, r.max.x, r.max.y]
}

/// A rectangle from the wire; anything that is no number becomes nothing.
pub fn rect_from(r: [f32; 4]) -> Rect {
    if r.iter().all(|v| v.is_finite()) {
        Rect::from_min_max(Pos2::new(r[0], r[1]), Pos2::new(r[2], r[3]))
    } else {
        Rect::NOTHING
    }
}

// ---- input --------------------------------------------------------------

pub fn modifiers_to(m: egui::Modifiers) -> wire::Modifiers {
    wire::Modifiers {
        alt: m.alt,
        ctrl: m.ctrl,
        shift: m.shift,
        mac_cmd: m.mac_cmd,
        command: m.command,
    }
}

pub fn modifiers_from(m: wire::Modifiers) -> egui::Modifiers {
    egui::Modifiers {
        alt: m.alt,
        ctrl: m.ctrl,
        shift: m.shift,
        mac_cmd: m.mac_cmd,
        command: m.command,
    }
}

const BUTTONS: [egui::PointerButton; 5] = [
    egui::PointerButton::Primary,
    egui::PointerButton::Secondary,
    egui::PointerButton::Middle,
    egui::PointerButton::Extra1,
    egui::PointerButton::Extra2,
];

const PHASES: [egui::TouchPhase; 4] = [
    egui::TouchPhase::Start,
    egui::TouchPhase::Move,
    egui::TouchPhase::End,
    egui::TouchPhase::Cancel,
];

fn phase_to(p: egui::TouchPhase) -> u8 {
    PHASES.iter().position(|x| *x == p).unwrap_or(1) as u8
}

fn phase_from(p: u8) -> egui::TouchPhase {
    PHASES.get(usize::from(p)).copied().unwrap_or(egui::TouchPhase::Move)
}

/// An event of the program's window as a plugin gets it; `None` for one that
/// is not for plugins.
pub fn event_to(e: &egui::Event) -> Option<wire::Event> {
    use wire::Event as W;
    Some(match e {
        egui::Event::PointerMoved(p) => W::PointerMoved { x: p.x, y: p.y },
        egui::Event::PointerButton {
            pos,
            button,
            pressed,
            modifiers,
        } => W::PointerButton {
            x: pos.x,
            y: pos.y,
            button: BUTTONS.iter().position(|b| b == button).unwrap_or(0) as u8,
            pressed: *pressed,
            modifiers: modifiers_to(*modifiers),
        },
        egui::Event::PointerGone => W::PointerGone {},
        egui::Event::MouseWheel {
            unit,
            delta,
            phase,
            modifiers,
            ..
        } => W::Wheel {
            unit: match unit {
                egui::MouseWheelUnit::Point => 0,
                egui::MouseWheelUnit::Line => 1,
                egui::MouseWheelUnit::Page => 2,
            },
            x: delta.x,
            y: delta.y,
            phase: phase_to(*phase),
            modifiers: modifiers_to(*modifiers),
        },
        egui::Event::Zoom(factor) => W::Zoom { factor: *factor },
        egui::Event::Key {
            key,
            physical_key,
            pressed,
            repeat,
            modifiers,
        } => W::Key {
            key: key.name().to_string(),
            physical: physical_key.map(|k| k.name().to_string()),
            pressed: *pressed,
            repeat: *repeat,
            modifiers: modifiers_to(*modifiers),
        },
        egui::Event::Text(text) => W::Text { text: text.clone() },
        egui::Event::Paste(text) => W::Paste { text: text.clone() },
        egui::Event::Copy => W::Copy {},
        egui::Event::Cut => W::Cut {},
        egui::Event::Ime(ime) => match ime {
            egui::ImeEvent::Preedit { text, .. } => W::ImePreedit { text: text.clone() },
            egui::ImeEvent::Commit(text) => W::ImeCommit { text: text.clone() },
            #[allow(deprecated)]
            egui::ImeEvent::Enabled => W::ImeEnabled {},
            #[allow(deprecated)]
            egui::ImeEvent::Disabled => W::ImeDisabled {},
            #[allow(unreachable_patterns)]
            _ => return None,
        },
        egui::Event::Touch {
            device_id,
            id,
            phase,
            pos,
            force,
        } => W::Touch {
            device: device_id.0,
            id: id.0,
            phase: phase_to(*phase),
            x: pos.x,
            y: pos.y,
            force: *force,
        },
        egui::Event::WindowFocused(focused) => W::WindowFocused { focused: *focused },
        _ => return None,
    })
}

/// The event as the plugin's egui takes it.
pub fn event_from(e: wire::Event) -> Option<egui::Event> {
    use wire::Event as W;
    Some(match e {
        W::PointerMoved { x, y } => egui::Event::PointerMoved(Pos2::new(x, y)),
        W::PointerButton {
            x,
            y,
            button,
            pressed,
            modifiers,
        } => egui::Event::PointerButton {
            pos: Pos2::new(x, y),
            button: *BUTTONS.get(usize::from(button))?,
            pressed,
            modifiers: modifiers_from(modifiers),
        },
        W::PointerGone {} => egui::Event::PointerGone,
        W::Wheel {
            unit,
            x,
            y,
            phase,
            modifiers,
        } => egui::Event::MouseWheel {
            unit: match unit {
                1 => egui::MouseWheelUnit::Line,
                2 => egui::MouseWheelUnit::Page,
                _ => egui::MouseWheelUnit::Point,
            },
            delta: egui::vec2(x, y),
            phase: phase_from(phase),
            source: egui::MouseWheelSource::Unknown,
            modifiers: modifiers_from(modifiers),
        },
        W::Zoom { factor } => egui::Event::Zoom(factor),
        W::Key {
            key,
            physical,
            pressed,
            repeat,
            modifiers,
        } => egui::Event::Key {
            key: egui::Key::from_name(&key)?,
            physical_key: physical.as_deref().and_then(egui::Key::from_name),
            pressed,
            repeat,
            modifiers: modifiers_from(modifiers),
        },
        W::Text { text } => egui::Event::Text(text),
        W::Paste { text } => egui::Event::Paste(text),
        W::Copy {} => egui::Event::Copy,
        W::Cut {} => egui::Event::Cut,
        #[allow(deprecated)]
        W::ImeEnabled {} => egui::Event::Ime(egui::ImeEvent::Enabled),
        W::ImePreedit { text } => egui::Event::Ime(egui::ImeEvent::Preedit {
            text,
            active_range_chars: None,
        }),
        W::ImeCommit { text } => egui::Event::Ime(egui::ImeEvent::Commit(text)),
        #[allow(deprecated)]
        W::ImeDisabled {} => egui::Event::Ime(egui::ImeEvent::Disabled),
        W::Touch {
            device,
            id,
            phase,
            x,
            y,
            force,
        } => egui::Event::Touch {
            device_id: egui::TouchDeviceId(device),
            id: egui::TouchId(id),
            phase: phase_from(phase),
            pos: Pos2::new(x, y),
            force,
        },
        W::WindowFocused { focused } => egui::Event::WindowFocused(focused),
    })
}

// ---- the pointer's shape ------------------------------------------------

const CURSORS: &[(CursorIcon, &str)] = &[
    (CursorIcon::Default, "default"),
    (CursorIcon::None, "none"),
    (CursorIcon::ContextMenu, "context-menu"),
    (CursorIcon::Help, "help"),
    (CursorIcon::PointingHand, "pointer"),
    (CursorIcon::Progress, "progress"),
    (CursorIcon::Wait, "wait"),
    (CursorIcon::Cell, "cell"),
    (CursorIcon::Crosshair, "crosshair"),
    (CursorIcon::Text, "text"),
    (CursorIcon::VerticalText, "vertical-text"),
    (CursorIcon::Alias, "alias"),
    (CursorIcon::Copy, "copy"),
    (CursorIcon::Move, "move"),
    (CursorIcon::NoDrop, "no-drop"),
    (CursorIcon::NotAllowed, "not-allowed"),
    (CursorIcon::Grab, "grab"),
    (CursorIcon::Grabbing, "grabbing"),
    (CursorIcon::AllScroll, "all-scroll"),
    (CursorIcon::ResizeHorizontal, "ew-resize"),
    (CursorIcon::ResizeNeSw, "nesw-resize"),
    (CursorIcon::ResizeNwSe, "nwse-resize"),
    (CursorIcon::ResizeVertical, "ns-resize"),
    (CursorIcon::ResizeEast, "e-resize"),
    (CursorIcon::ResizeSouthEast, "se-resize"),
    (CursorIcon::ResizeSouth, "s-resize"),
    (CursorIcon::ResizeSouthWest, "sw-resize"),
    (CursorIcon::ResizeWest, "w-resize"),
    (CursorIcon::ResizeNorthWest, "nw-resize"),
    (CursorIcon::ResizeNorth, "n-resize"),
    (CursorIcon::ResizeNorthEast, "ne-resize"),
    (CursorIcon::ResizeColumn, "col-resize"),
    (CursorIcon::ResizeRow, "row-resize"),
    (CursorIcon::ZoomIn, "zoom-in"),
    (CursorIcon::ZoomOut, "zoom-out"),
];

pub fn cursor_name(c: CursorIcon) -> &'static str {
    CURSORS.iter().find(|(i, _)| *i == c).map_or("default", |(_, n)| n)
}

pub fn cursor_from(name: &str) -> CursorIcon {
    CURSORS
        .iter()
        .find(|(_, n)| *n == name)
        .map_or(CursorIcon::Default, |(i, _)| *i)
}

// ---- paint --------------------------------------------------------------

/// A mesh as bytes.
pub fn mesh_to(mesh: &epaint::Mesh, clip: Rect, texture: u64) -> wire::Mesh {
    let mut vertices = Vec::with_capacity(mesh.vertices.len() * wire::VERTEX_BYTES);
    for v in &mesh.vertices {
        for f in [v.pos.x, v.pos.y, v.uv.x, v.uv.y] {
            vertices.extend_from_slice(&f.to_le_bytes());
        }
        vertices.extend_from_slice(&v.color.to_array());
    }
    let mut indices = Vec::with_capacity(mesh.indices.len() * 4);
    for i in &mesh.indices {
        indices.extend_from_slice(&i.to_le_bytes());
    }
    wire::Mesh {
        clip: rect_to(clip),
        texture,
        vertices: ByteBuf::from(vertices),
        indices: ByteBuf::from(indices),
    }
}

/// The mesh of the bytes, drawn with `texture`. `None` when they are not a
/// mesh: cut short, or an index that points at no vertex, which the renderer
/// must never see.
pub fn mesh_from(m: &wire::Mesh, texture: egui::TextureId) -> Option<epaint::Mesh> {
    if !m.vertices.len().is_multiple_of(wire::VERTEX_BYTES) || !m.indices.len().is_multiple_of(12) {
        return None;
    }
    let float = |c: &[u8]| {
        let v = f32::from_le_bytes([c[0], c[1], c[2], c[3]]);
        if v.is_finite() {
            v
        } else {
            0.0
        }
    };
    let vertices: Vec<epaint::Vertex> = m
        .vertices
        .as_chunks::<{ wire::VERTEX_BYTES }>()
        .0
        .iter()
        .map(|c| epaint::Vertex {
            pos: Pos2::new(float(&c[0..4]), float(&c[4..8])),
            uv: Pos2::new(float(&c[8..12]), float(&c[12..16])),
            color: Color32::from_rgba_premultiplied(c[16], c[17], c[18], c[19]),
        })
        .collect();
    let indices: Vec<u32> = m
        .indices
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| u32::from_le_bytes(*c))
        .collect();
    if indices.iter().any(|&i| i as usize >= vertices.len()) {
        return None;
    }
    Some(epaint::Mesh {
        indices,
        vertices,
        texture_id: texture,
    })
}

pub fn texture_to(id: u64, delta: &epaint::ImageDelta) -> wire::Texture {
    let epaint::ImageData::Color(image) = &delta.image;
    let mut rgba = Vec::with_capacity(image.pixels.len() * 4);
    for p in &image.pixels {
        rgba.extend_from_slice(&p.to_array());
    }
    let linear = |f: epaint::textures::TextureFilter| f == epaint::textures::TextureFilter::Linear;
    wire::Texture {
        id,
        pos: delta.pos.map(|p| [p[0] as u32, p[1] as u32]),
        size: [image.size[0] as u32, image.size[1] as u32],
        rgba: ByteBuf::from(rgba),
        linear_magnify: linear(delta.options.magnification),
        linear_minify: linear(delta.options.minification),
        repeat: delta.options.wrap_mode != epaint::textures::TextureWrapMode::ClampToEdge,
    }
}

/// The pixels and the sampling of a texture; `None` when the bytes are not as
/// many as the size says, or the size is more than `max_side`.
pub fn texture_from(t: &wire::Texture, max_side: u32) -> Option<(egui::ColorImage, egui::TextureOptions)> {
    let [w, h] = t.size;
    if w == 0 || h == 0 || w > max_side || h > max_side || t.rgba.len() as u64 != u64::from(w) * u64::from(h) * 4 {
        return None;
    }
    let pixels = t
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| Color32::from_rgba_premultiplied(c[0], c[1], c[2], c[3]))
        .collect();
    let filter = |linear| {
        if linear {
            egui::TextureFilter::Linear
        } else {
            egui::TextureFilter::Nearest
        }
    };
    let options = egui::TextureOptions {
        magnification: filter(t.linear_magnify),
        minification: filter(t.linear_minify),
        wrap_mode: if t.repeat {
            egui::TextureWrapMode::Repeat
        } else {
            egui::TextureWrapMode::ClampToEdge
        },
        mipmap_mode: None,
    };
    Some((egui::ColorImage::new([w as usize, h as usize], pixels), options))
}

// ---- the plugin's own types ---------------------------------------------

pub fn map_event_to(e: &MapEvent) -> wire::MapEvent {
    let (kind, key, at) = match e {
        MapEvent::Click(k) => ("click", k.clone(), [0.0; 2]),
        MapEvent::DoubleClick(k) => ("double_click", k.clone(), [0.0; 2]),
        MapEvent::Drag(k, at) => ("drag", k.clone(), *at),
        MapEvent::DragEnd(k, at) => ("drag_end", k.clone(), *at),
        MapEvent::Picked(at) => ("picked", String::new(), *at),
        MapEvent::PickCancelled => ("pick_cancelled", String::new(), [0.0; 2]),
    };
    wire::MapEvent {
        kind: kind.into(),
        key,
        at,
    }
}

/// What the program read of a folder, for the plugin that asked; `None` when the
/// operator picked none.
pub fn scanned_to(tag: u64, scan: Option<FolderScan>) -> wire::Scanned {
    let cancelled = scan.is_none();
    let scan = scan.unwrap_or_default();
    wire::Scanned {
        tag,
        cancelled,
        files: scan
            .files
            .into_iter()
            .map(|f| wire::PlacedFile {
                path: f.path,
                lat: f.lat,
                lng: f.lng,
                alt: f.alt,
                taken: f.taken,
            })
            .collect(),
        without_place: scan.without_place,
        videos_unread: scan.videos_unread,
        cut: scan.cut,
        error: scan.error,
    }
}

pub fn scanned_from(s: wire::Scanned) -> (u64, Option<FolderScan>) {
    let scan = (!s.cancelled).then(|| FolderScan {
        files: s
            .files
            .into_iter()
            .map(|f| Placed {
                path: f.path,
                lat: f.lat,
                lng: f.lng,
                alt: f.alt,
                taken: f.taken,
            })
            .collect(),
        without_place: s.without_place,
        videos_unread: s.videos_unread,
        cut: s.cut,
        error: s.error,
    });
    (s.tag, scan)
}

pub fn map_event_from(e: wire::MapEvent) -> Option<MapEvent> {
    Some(match e.kind.as_str() {
        "click" => MapEvent::Click(e.key),
        "double_click" => MapEvent::DoubleClick(e.key),
        "drag" => MapEvent::Drag(e.key, e.at),
        "drag_end" => MapEvent::DragEnd(e.key, e.at),
        "picked" => MapEvent::Picked(e.at),
        "pick_cancelled" => MapEvent::PickCancelled,
        _ => return None,
    })
}

pub fn tool_to(t: McpTool) -> wire::McpTool {
    wire::McpTool {
        name: t.name,
        title: t.title,
        description: t.description,
        input_schema: t.input_schema,
        confirm: t.confirm,
        read_only: t.read_only,
    }
}

pub fn tool_from(t: wire::McpTool) -> McpTool {
    McpTool {
        name: t.name,
        title: t.title,
        description: t.description,
        input_schema: t.input_schema,
        confirm: t.confirm,
        read_only: t.read_only,
    }
}

pub fn option_to(s: OptionSpec) -> wire::OptionSpec {
    let mut out = wire::OptionSpec {
        key: s.key,
        label: s.label,
        hint: s.hint,
        default: s.default,
        secret: s.secret,
        ..wire::OptionSpec::default()
    };
    out.kind = match s.kind {
        Kind::Toggle => "toggle",
        Kind::Select(choices) => {
            out.choices = choices;
            "select"
        }
        Kind::Text { placeholder } => {
            out.placeholder = placeholder;
            "text"
        }
        Kind::Number { min, max, step } => {
            (out.min, out.max, out.step) = (min, max, step);
            "number"
        }
    }
    .into();
    out
}

/// `None` for a kind of option this side does not know.
pub fn option_from(s: wire::OptionSpec) -> Option<OptionSpec> {
    let kind = match s.kind.as_str() {
        "toggle" => Kind::Toggle,
        "select" => Kind::Select(s.choices),
        "text" => Kind::Text {
            placeholder: s.placeholder,
        },
        "number" => Kind::Number {
            min: s.min,
            max: s.max,
            step: s.step,
        },
        _ => return None,
    };
    Some(OptionSpec {
        key: s.key,
        label: s.label,
        hint: s.hint,
        kind,
        default: s.default,
        secret: s.secret,
    })
}

pub fn shape_to(s: &Shape) -> wire::Shape {
    use shapes::Kind as K;
    let mut out = wire::Shape {
        key: s.key.clone(),
        color: s.color.to_array(),
        width: s.width,
        opacity: s.opacity,
        visible: s.visible,
        ..wire::Shape::default()
    };
    out.kind = match &s.kind {
        K::Line { pts, dashed } => {
            out.points = pts.clone();
            out.dashed = *dashed;
            "line"
        }
        K::Polygon {
            pts,
            fill_opacity,
            outline,
            dashed,
        } => {
            out.points = pts.clone();
            (out.fill_opacity, out.outline, out.dashed) = (*fill_opacity, *outline, *dashed);
            "polygon"
        }
        K::Text { at, text, size, dot } => {
            out.points = vec![*at];
            (out.text, out.size, out.dot) = (text.clone(), *size, *dot);
            "text"
        }
        K::Button {
            at,
            text,
            title,
            size,
            disabled,
        } => {
            out.points = vec![*at];
            (out.text, out.title, out.size) = (text.clone(), title.clone(), *size);
            out.disabled = *disabled;
            "button"
        }
        K::Marker {
            at,
            icon,
            text,
            title,
            size,
            selected,
            draggable,
            clickable,
        } => {
            out.points = vec![*at];
            out.icon = icon.clone();
            (out.text, out.title, out.size) = (text.clone(), title.clone(), *size);
            (out.selected, out.draggable, out.clickable) = (*selected, *draggable, *clickable);
            "marker"
        }
    }
    .into();
    out
}

/// Draws a plugin's shape into `set`, through the same calls and so the same
/// checks as a shape of the program's own: a position that is none, a line of
/// one point or a text without end never reaches the map.
pub fn shape_into(set: &mut shapes::ShapeSet, s: wire::Shape) {
    let [r, g, b, a] = s.color;
    let style = shapes::Style {
        color: Color32::from_rgba_premultiplied(r, g, b, a),
        opacity: s.opacity.is_finite().then(|| s.opacity.clamp(0.0, 1.0)),
        visible: s.visible,
    };
    let finite = |v: f32| if v.is_finite() { v } else { 0.0 };
    let at = s.points.first().copied().unwrap_or([f64::NAN; 2]);
    match s.kind.as_str() {
        "line" => {
            set.line(&s.key, &s.points, style, Some(finite(s.width)), s.dashed);
        }
        "polygon" => {
            let (fill, outline) = (Some(finite(s.fill_opacity)), Some(finite(s.outline)));
            set.polygon(&s.key, &s.points, style, fill, outline, s.dashed);
        }
        "text" => {
            set.text(&s.key, at, &s.text, style, Some(finite(s.size)), s.dot);
        }
        "button" => {
            set.button(&s.key, at, &s.text, &s.title, style, Some(finite(s.size)), s.disabled);
        }
        "marker" => {
            let size = finite(s.size);
            set.restore_marker(
                &s.key,
                shapes::Kind::Marker {
                    at,
                    icon: s.icon.into_iter().take(8).collect(),
                    text: s.text,
                    title: s.title,
                    size,
                    selected: s.selected,
                    draggable: s.draggable,
                    clickable: s.clickable,
                },
                style,
            );
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_event_survives_the_way_to_a_plugin() {
        let sent = [
            egui::Event::PointerMoved(Pos2::new(3.0, 4.0)),
            egui::Event::Text("a\u{e4}".into()),
            egui::Event::Key {
                key: egui::Key::ArrowLeft,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::SHIFT,
            },
            egui::Event::Paste("x".into()),
            egui::Event::PointerGone,
        ];
        for e in sent {
            assert_eq!(event_from(event_to(&e).unwrap()), Some(e));
        }
    }

    #[test]
    fn every_key_is_found_again_by_its_name() {
        for k in egui::Key::ALL {
            assert_eq!(egui::Key::from_name(k.name()), Some(*k));
        }
    }

    #[test]
    fn a_mesh_survives_and_a_damaged_one_is_refused() {
        let mut mesh = epaint::Mesh::default();
        for x in 0..3 {
            mesh.vertices.push(epaint::Vertex {
                pos: Pos2::new(x as f32, 1.0),
                uv: Pos2::new(0.5, 0.25),
                color: Color32::from_rgba_premultiplied(1, 2, 3, 4),
            });
        }
        mesh.indices = vec![0, 1, 2];
        let sent = mesh_to(&mesh, Rect::from_min_max(Pos2::ZERO, Pos2::new(9.0, 9.0)), 7);
        let back = mesh_from(&sent, egui::TextureId::Managed(7)).unwrap();
        assert_eq!(back.vertices, mesh.vertices);
        assert_eq!(back.indices, mesh.indices);
        let mut far = sent.clone();
        far.indices = ByteBuf::from([3u32, 0, 1].iter().flat_map(|i| i.to_le_bytes()).collect::<Vec<_>>());
        assert!(
            mesh_from(&far, egui::TextureId::Managed(0)).is_none(),
            "an index past the end"
        );
        let mut cut = sent;
        cut.vertices.pop();
        assert!(mesh_from(&cut, egui::TextureId::Managed(0)).is_none());
    }

    #[test]
    fn a_texture_must_be_as_large_as_it_says() {
        let t = wire::Texture {
            size: [2, 2],
            rgba: ByteBuf::from(vec![0; 16]),
            ..wire::Texture::default()
        };
        assert!(texture_from(&t, 2048).is_some());
        assert!(texture_from(&t, 1).is_none());
        let short = wire::Texture {
            rgba: ByteBuf::from(vec![0; 12]),
            ..t
        };
        assert!(texture_from(&short, 2048).is_none());
    }
}
