//! Frosted glass: CSS `backdrop-filter: blur(..) saturate(..)`, which every
//! `.glass` surface of the web interface carries (`--blur-soft: blur(24px)
//! saturate(180%)`, components.css, themes.css).
//!
//! egui has no backdrop filter, so this is a paint callback on the OpenGL
//! renderer. When egui reaches it, everything under the panel (the map,
//! the track, the markers) is already in the framebuffer. The callback
//! copies that region with a margin, builds its mipmaps, and draws it back
//! inside the panel's rounded rect through a Gaussian of the CSS radius and
//! the CSS saturate matrix. The panel's own fill and content then paint
//! over it as they always did, so the result is the browser's: blur and
//! saturate the backdrop, then the translucent white on top.
//!
//! The filter works on the framebuffer's sRGB values, as browsers do.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use eframe::egui::{self, CornerRadius, Rect};
use eframe::egui_glow;
use eframe::glow::{self, HasContext};

/// `--blur-soft` of the theme in force: what `.glass`, the map rail and the
/// attribution pill use. The light themes blur 24px and saturate 180%; the
/// dark ones saturate 190% and brighten by 10%, which is what lets the frost
/// come through over a dark surface.
pub fn soft() -> Filter {
    filter_of(&crate::style::palette())
}

/// `--blur-soft` of a palette.
pub fn filter_of(p: &crate::style::Palette) -> Filter {
    let (blur, saturate, brightness) = p.blur_soft;
    Filter {
        blur,
        saturate,
        brightness,
    }
}

/// One backdrop filter: a Gaussian of `blur` CSS px, then `saturate`, then
/// `brightness`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Filter {
    pub blur: f32,
    pub saturate: f32,
    pub brightness: f32,
}

struct Resources {
    program: glow::Program,
    vao: glow::VertexArray,
    /// One texture per panel: the backdrop is copied before the panel's
    /// shadow is drawn and filtered back after it, because a browser draws
    /// a box-shadow outside the box only, and egui's would darken the copy.
    textures: HashMap<u64, (glow::Texture, [i32; 4])>,
    u_region: Option<glow::UniformLocation>,
    u_rect: Option<glow::UniformLocation>,
    u_radius: Option<glow::UniformLocation>,
    u_blur: Option<glow::UniformLocation>,
    u_sat: Option<glow::UniformLocation>,
    u_bright: Option<glow::UniformLocation>,
    u_tex: Option<glow::UniformLocation>,
}

/// None until first used; `Err` when this GL cannot run the shader, in
/// which case the panels keep their plain translucent fill.
static RESOURCES: Mutex<Option<Result<Resources, String>>> = Mutex::new(None);

const VERTEX: &str = r#"
void main() {
    // A full viewport quad from the vertex index; the viewport is the panel.
    vec2 p = vec2(float(gl_VertexID & 1), float((gl_VertexID >> 1) & 1));
    gl_Position = vec4(p * 2.0 - 1.0, 0.0, 1.0);
}
"#;

const FRAGMENT: &str = r#"
uniform sampler2D u_tex;
uniform vec4 u_region; // the copied region: x, y (from the bottom), w, h in px
uniform vec4 u_rect;   // the panel: x, y (from the bottom), w, h in px
uniform vec4 u_radius; // corner radii in px: top left, top right, bottom right, bottom left
uniform float u_blur;  // the Gaussian's sigma in px
uniform float u_sat;   // the CSS saturate amount
uniform float u_bright; // the CSS brightness amount
out vec4 frag;

float rounded(vec2 p, vec2 half_size, vec4 r) {
    float radius = p.x < 0.0 ? (p.y > 0.0 ? r.x : r.w) : (p.y > 0.0 ? r.y : r.z);
    radius = min(radius, min(half_size.x, half_size.y));
    vec2 q = abs(p) - half_size + radius;
    return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - radius;
}

void main() {
    vec2 px = gl_FragCoord.xy;
    vec2 local = px - u_rect.xy - u_rect.zw * 0.5;
    float coverage = clamp(0.5 - rounded(local, u_rect.zw * 0.5, u_radius), 0.0, 1.0);
    if (coverage <= 0.0) discard;

    // 9 x 9 taps half a sigma apart, two sigma either way, read from the
    // mip level whose texels are a third of a sigma, so the taps blend.
    float lod = max(0.0, log2(u_blur / 3.0));
    float step = u_blur * 0.5;
    vec3 sum = vec3(0.0);
    float weight = 0.0;
    for (int j = -4; j <= 4; j++) {
        for (int i = -4; i <= 4; i++) {
            vec2 off = vec2(float(i), float(j)) * step;
            float w = exp(-dot(off, off) / (2.0 * u_blur * u_blur));
            vec2 uv = clamp((px + off - u_region.xy) / u_region.zw, vec2(0.0), vec2(1.0));
            sum += textureLod(u_tex, uv, lod).rgb * w;
            weight += w;
        }
    }
    vec3 c = sum / weight;

    // CSS saturate(): the filter-effects matrix.
    float s = u_sat;
    mat3 m = mat3(
        0.213 + 0.787 * s, 0.213 - 0.213 * s, 0.213 - 0.213 * s,
        0.715 - 0.715 * s, 0.715 + 0.285 * s, 0.715 - 0.715 * s,
        0.072 - 0.072 * s, 0.072 - 0.072 * s, 0.072 + 0.928 * s
    );
    c = clamp(m * c * u_bright, 0.0, 1.0);
    // egui blends premultiplied.
    frag = vec4(c * coverage, coverage);
}
"#;

unsafe fn compile(gl: &glow::Context, kind: u32, header: &str, body: &str) -> Result<glow::Shader, String> {
    let shader = gl.create_shader(kind)?;
    gl.shader_source(shader, &format!("{header}\nprecision highp float;\n{body}"));
    gl.compile_shader(shader);
    if gl.get_shader_compile_status(shader) {
        Ok(shader)
    } else {
        let log = gl.get_shader_info_log(shader);
        gl.delete_shader(shader);
        Err(log)
    }
}

unsafe fn create(gl: &glow::Context) -> Result<Resources, String> {
    let version = egui_glow::ShaderVersion::get(gl);
    if !version.is_new_shader_interface() {
        return Err("GLSL without `in`/`out`: no frosted glass".into());
    }
    let header = version.version_declaration();
    let vs = compile(gl, glow::VERTEX_SHADER, header, VERTEX)?;
    let fs = compile(gl, glow::FRAGMENT_SHADER, header, FRAGMENT)?;
    let program = gl.create_program()?;
    gl.attach_shader(program, vs);
    gl.attach_shader(program, fs);
    gl.link_program(program);
    gl.detach_shader(program, vs);
    gl.detach_shader(program, fs);
    gl.delete_shader(vs);
    gl.delete_shader(fs);
    if !gl.get_program_link_status(program) {
        let log = gl.get_program_info_log(program);
        gl.delete_program(program);
        return Err(log);
    }
    let vao = gl.create_vertex_array()?;
    let u = |name: &str| gl.get_uniform_location(program, name);
    Ok(Resources {
        u_region: u("u_region"),
        u_rect: u("u_rect"),
        u_radius: u("u_radius"),
        u_blur: u("u_blur"),
        u_sat: u("u_sat"),
        u_bright: u("u_bright"),
        u_tex: u("u_tex"),
        program,
        vao,
        textures: HashMap::new(),
    })
}

/// The region to copy: the panel and `margin` px around it, inside the
/// screen. `(x, y_from_bottom, w, h)`, or None when nothing is on screen.
pub fn region(panel: [i32; 4], margin: i32, screen: [u32; 2]) -> Option<[i32; 4]> {
    let [x, y, w, h] = panel;
    let x0 = (x - margin).max(0);
    let y0 = (y - margin).max(0);
    let x1 = (x + w + margin).min(screen[0] as i32);
    let y1 = (y + h + margin).min(screen[1] as i32);
    (x1 > x0 && y1 > y0).then_some([x0, y0, x1 - x0, y1 - y0])
}

fn resources(gl: &glow::Context) -> std::sync::MutexGuard<'static, Option<Result<Resources, String>>> {
    let mut guard = RESOURCES.lock().unwrap_or_else(|e| e.into_inner());
    if guard.is_none() {
        // SAFETY: on egui_glow's context, inside its paint.
        let made = unsafe { create(gl) };
        if let Err(e) = &made {
            eprintln!("corvus-gcs: no frosted glass, the panels stay plain: {e}");
        }
        *guard = Some(made);
    }
    guard
}

fn margin(info: &egui::PaintCallbackInfo, filter: Filter) -> (f32, i32) {
    let sigma = (filter.blur * info.pixels_per_point).max(1.0);
    (sigma, (sigma * 2.0).ceil() as i32)
}

/// Copies what is on screen under the panel `id` now, with the margin the
/// blur reads.
unsafe fn copy(info: &egui::PaintCallbackInfo, gl: &glow::Context, id: u64, filter: Filter) {
    let mut guard = resources(gl);
    let Some(Ok(res)) = guard.as_mut() else { return };
    let vp = info.viewport_in_pixels();
    let (_, m) = margin(info, filter);
    let panel = [vp.left_px, vp.from_bottom_px, vp.width_px, vp.height_px];
    let Some(reg) = region(panel, m, info.screen_size_px) else {
        res.textures.remove(&id);
        return;
    };
    let texture = match res.textures.get(&id) {
        Some((t, _)) => *t,
        None => match gl.create_texture() {
            Ok(t) => t,
            Err(_) => return,
        },
    };
    gl.active_texture(glow::TEXTURE0);
    gl.bind_texture(glow::TEXTURE_2D, Some(texture));
    gl.copy_tex_image_2d(glow::TEXTURE_2D, 0, glow::RGBA8, reg[0], reg[1], reg[2], reg[3], 0);
    gl.generate_mipmap(glow::TEXTURE_2D);
    gl.tex_parameter_i32(
        glow::TEXTURE_2D,
        glow::TEXTURE_MIN_FILTER,
        glow::LINEAR_MIPMAP_LINEAR as i32,
    );
    gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, glow::LINEAR as i32);
    gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE as i32);
    gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE as i32);
    res.textures.insert(id, (texture, reg));
}

/// Draws the copy of panel `id` back inside its rounded rect, filtered.
unsafe fn draw(info: &egui::PaintCallbackInfo, gl: &glow::Context, id: u64, radius: CornerRadius, filter: Filter) {
    let mut guard = resources(gl);
    let Some(Ok(res)) = guard.as_mut() else { return };
    let Some(&(texture, [rx, ry, rw, rh])) = res.textures.get(&id) else {
        return;
    };
    let ppp = info.pixels_per_point;
    let vp = info.viewport_in_pixels();
    let (sigma, _) = margin(info, filter);
    let panel = [vp.left_px, vp.from_bottom_px, vp.width_px, vp.height_px];
    gl.active_texture(glow::TEXTURE0);
    gl.bind_texture(glow::TEXTURE_2D, Some(texture));
    gl.use_program(Some(res.program));
    gl.uniform_1_i32(res.u_tex.as_ref(), 0);
    gl.uniform_4_f32(res.u_region.as_ref(), rx as f32, ry as f32, rw as f32, rh as f32);
    gl.uniform_4_f32(
        res.u_rect.as_ref(),
        panel[0] as f32,
        panel[1] as f32,
        panel[2] as f32,
        panel[3] as f32,
    );
    let r = |v: u8| f32::from(v) * ppp;
    gl.uniform_4_f32(
        res.u_radius.as_ref(),
        r(radius.nw),
        r(radius.ne),
        r(radius.se),
        r(radius.sw),
    );
    gl.uniform_1_f32(res.u_blur.as_ref(), sigma);
    gl.uniform_1_f32(res.u_sat.as_ref(), filter.saturate);
    gl.uniform_1_f32(res.u_bright.as_ref(), filter.brightness);
    gl.bind_vertex_array(Some(res.vao));
    gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
    gl.bind_vertex_array(None);
}

fn callback(
    rect: Rect,
    f: impl Fn(egui::PaintCallbackInfo, &glow::Context) + Send + Sync + 'static,
) -> egui::PaintCallback {
    let cb = egui_glow::CallbackFn::new(move |info, p| f(info, p.gl()));
    egui::PaintCallback {
        rect,
        callback: Arc::new(cb),
    }
}

/// The first half of a backdrop filter: copies what lies under `rect`.
/// Add it before the panel's shadow.
pub fn copy_backdrop(painter: &egui::Painter, id: u64, rect: Rect, filter: Filter) {
    // SAFETY: egui_glow calls this on its own context between its draws and
    // restores its state after it.
    painter.add(callback(rect, move |info, gl| unsafe { copy(&info, gl, id, filter) }));
}

/// The second half: the copy, blurred and saturated, inside `rect`'s
/// rounded corners. Add it after the shadow and before the panel's fill.
pub fn draw_backdrop(painter: &egui::Painter, id: u64, rect: Rect, radius: CornerRadius, filter: Filter) {
    // SAFETY: as in copy_backdrop.
    painter.add(callback(rect, move |info, gl| unsafe {
        draw(&info, gl, id, radius, filter)
    }));
}

/// Frees the GL objects. Called with the context on the way out.
pub fn destroy(gl: &glow::Context) {
    let mut guard = RESOURCES.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(Ok(res)) = guard.take() {
        // SAFETY: the objects were made on this context and nothing uses
        // them once the window is closing.
        unsafe {
            gl.delete_program(res.program);
            gl.delete_vertex_array(res.vao);
            for (texture, _) in res.textures.into_values() {
                gl.delete_texture(texture);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_copied_region_stays_on_screen() {
        assert_eq!(region([100, 100, 50, 40], 10, [1000, 800]), Some([90, 90, 70, 60]));
        assert_eq!(region([5, 5, 50, 40], 10, [1000, 800]), Some([0, 0, 65, 55]));
        assert_eq!(region([980, 790, 50, 40], 10, [1000, 800]), Some([970, 780, 30, 20]));
        assert_eq!(region([2000, 0, 10, 10], 5, [1000, 800]), None);
    }

    #[test]
    fn the_glass_filter_is_the_css_one_per_theme() {
        use crate::style::palette_of;
        assert_eq!(
            filter_of(&palette_of("light")),
            Filter {
                blur: 24.0,
                saturate: 1.8,
                brightness: 1.0
            }
        );
        assert_eq!(
            filter_of(&palette_of("blue")),
            Filter {
                blur: 24.0,
                saturate: 1.9,
                brightness: 1.1
            }
        );
    }
}
