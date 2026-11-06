//! The 3D map's renderer: the ground and the buildings drawn by OpenGL, in an
//! egui paint callback, with a depth buffer.
//!
//! Every mesh is built once on the CPU in metres around its own origin
//! (`MeshData`) and kept on the GPU as long as it is used. A frame is only
//! uniforms: the view projection of the camera, where each mesh's origin
//! lies against the view centre, and the texture to drape. The depth buffer
//! settles what hides what, so a ridge hides the valley behind it and a
//! building hides the one beyond, whatever order the draws come in.
//!
//! The shaders write sRGB values straight to the framebuffer and read the
//! imagery as stored, which is how the flat map and the browser composite.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use eframe::egui;
use eframe::egui_glow;
use eframe::glow::{self, HasContext};

/// Floats per vertex: position (3), texture coordinate (2), shade (1).
pub const FLOATS: usize = 6;
/// Frames a mesh or a texture may go unused before the GPU lets it go.
const KEEP_FRAMES: u64 = 240;
/// What the GPU holds at most. A frame draws no more than a few hundred
/// textures, but the age rule above frees one only after `KEEP_FRAMES`, and a
/// view that keeps asking for new tiles (a low pitch over a wide area) made
/// thousands of them live at once, 3 GB on the machine. Past the budget the
/// least recently drawn go first, never one drawn in the current frame.
const MAX_TEXTURES: usize = 1000;
const MAX_MESHES: usize = 4000;
/// Textures uploaded in one frame at most, so a pan does not stall on them.
const UPLOADS_PER_FRAME: usize = 24;

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// A fresh id for a mesh or a texture; an id is never reused, so the GPU can
/// key on it.
pub fn next_id() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

/// A mesh the GPU keeps: interleaved vertices (`FLOATS` each) and triangles.
/// Positions are east and north in web mercator metres from the mesh's
/// origin, and the height above sea level in metres.
pub struct MeshData {
    pub id: u64,
    pub verts: Vec<f32>,
    pub indices: Vec<u32>,
}

impl MeshData {
    pub fn new(verts: Vec<f32>, indices: Vec<u32>) -> MeshData {
        debug_assert!(verts.len().is_multiple_of(FLOATS));
        MeshData {
            id: next_id(),
            verts,
            indices,
        }
    }
}

/// An image on its way to a texture.
#[derive(Clone)]
pub struct Texture {
    pub id: u64,
    pub image: Arc<egui::ColorImage>,
}

/// One draw: a mesh at a place, with what covers it.
pub struct Item {
    pub mesh: Arc<MeshData>,
    /// Web mercator metres of the mesh's origin.
    pub origin: (f64, f64),
    pub texture: Option<Texture>,
    /// The part of the texture the mesh's `(0..1, 0..1)` maps to: x, y, w, h.
    pub uv: [f32; 4],
    /// Draw at the view centre's height, for ground with no elevation yet.
    pub flat: bool,
    /// What the vertex shade multiplies where there is no texture.
    pub color: [f32; 3],
    pub alpha: f32,
}

/// Everything one frame of the 3D view draws.
pub struct Scene {
    /// `Camera::view_projection`.
    pub mvp: [f32; 16],
    /// Web mercator metres of the view centre, and the scale of its latitude.
    pub centre: (f64, f64),
    pub k: f64,
    /// The height of the view centre: local `up` is measured from it.
    pub ground: f64,
    pub ground_items: Vec<Item>,
    pub building_items: Vec<Item>,
    /// The track and other lines: ribbons the depth test hides behind the
    /// ground, blended, a little nearer than the ground they lie on.
    pub line_items: Vec<Item>,
}

struct GlMesh {
    vao: glow::VertexArray,
    vbo: glow::Buffer,
    ebo: glow::Buffer,
    count: i32,
    used: u64,
}

struct GlTexture {
    texture: glow::Texture,
    used: u64,
}

struct Uniforms {
    mvp: Option<glow::UniformLocation>,
    off: Option<glow::UniformLocation>,
    k: Option<glow::UniformLocation>,
    flat: Option<glow::UniformLocation>,
    uv: Option<glow::UniformLocation>,
    color: Option<glow::UniformLocation>,
    alpha: Option<glow::UniformLocation>,
    use_tex: Option<glow::UniformLocation>,
    tex: Option<glow::UniformLocation>,
}

struct Gpu {
    program: glow::Program,
    uniforms: Uniforms,
    meshes: HashMap<u64, GlMesh>,
    textures: HashMap<u64, GlTexture>,
    frame: u64,
    anisotropy: Option<f32>,
}

/// None until first used. `Err` when this GL cannot run the shaders; the map
/// then says so instead of drawing.
static GPU: Mutex<Option<Result<Gpu, String>>> = Mutex::new(None);

const VERTEX: &str = r#"
uniform mat4 u_mvp;
uniform vec3 u_off;   // east, north in metres from the view centre, and minus its height
uniform float u_k;    // true metres per mercator metre at the centre
uniform float u_flat; // 1 draws at the centre's height
uniform vec4 u_uv;
in vec3 a_pos;
in vec2 a_uv;
in float a_shade;
out vec2 v_uv;
out float v_shade;
void main() {
    float up = u_flat > 0.5 ? 0.0 : a_pos.z + u_off.z;
    vec3 p = vec3(a_pos.xy * u_k + u_off.xy, up);
    gl_Position = u_mvp * vec4(p, 1.0);
    v_uv = u_uv.xy + a_uv * u_uv.zw;
    v_shade = a_shade;
}
"#;

const FRAGMENT: &str = r#"
uniform sampler2D u_tex;
uniform float u_use_tex;
uniform vec3 u_color;
uniform float u_alpha;
in vec2 v_uv;
in float v_shade;
out vec4 frag;
void main() {
    vec3 base = u_use_tex > 0.5 ? texture(u_tex, v_uv).rgb : u_color;
    frag = vec4(base * v_shade, u_alpha);
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

unsafe fn create(gl: &glow::Context) -> Result<Gpu, String> {
    let version = egui_glow::ShaderVersion::get(gl);
    if !version.is_new_shader_interface() {
        return Err("this OpenGL has no GLSL with `in` and `out`".into());
    }
    let header = version.version_declaration();
    let vs = compile(gl, glow::VERTEX_SHADER, header, VERTEX)?;
    let fs = compile(gl, glow::FRAGMENT_SHADER, header, FRAGMENT)?;
    let program = gl.create_program()?;
    gl.attach_shader(program, vs);
    gl.attach_shader(program, fs);
    gl.bind_attrib_location(program, 0, "a_pos");
    gl.bind_attrib_location(program, 1, "a_uv");
    gl.bind_attrib_location(program, 2, "a_shade");
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
    let u = |name: &str| gl.get_uniform_location(program, name);
    let anisotropy = gl
        .supported_extensions()
        .iter()
        .any(|e| e.ends_with("texture_filter_anisotropic"))
        .then(|| gl.get_parameter_f32(0x84FF).clamp(1.0, 8.0));
    Ok(Gpu {
        uniforms: Uniforms {
            mvp: u("u_mvp"),
            off: u("u_off"),
            k: u("u_k"),
            flat: u("u_flat"),
            uv: u("u_uv"),
            color: u("u_color"),
            alpha: u("u_alpha"),
            use_tex: u("u_use_tex"),
            tex: u("u_tex"),
        },
        program,
        meshes: HashMap::new(),
        textures: HashMap::new(),
        frame: 0,
        anisotropy,
    })
}

impl Gpu {
    unsafe fn upload_mesh(&mut self, gl: &glow::Context, mesh: &MeshData) -> Option<()> {
        if self.meshes.contains_key(&mesh.id) {
            return Some(());
        }
        if mesh.indices.is_empty() {
            return None;
        }
        let vao = gl.create_vertex_array().ok()?;
        let vbo = gl.create_buffer().ok()?;
        let ebo = gl.create_buffer().ok()?;
        gl.bind_vertex_array(Some(vao));
        gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
        gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, as_bytes(&mesh.verts), glow::STATIC_DRAW);
        gl.bind_buffer(glow::ELEMENT_ARRAY_BUFFER, Some(ebo));
        gl.buffer_data_u8_slice(glow::ELEMENT_ARRAY_BUFFER, as_bytes(&mesh.indices), glow::STATIC_DRAW);
        let stride = (FLOATS * 4) as i32;
        gl.enable_vertex_attrib_array(0);
        gl.vertex_attrib_pointer_f32(0, 3, glow::FLOAT, false, stride, 0);
        gl.enable_vertex_attrib_array(1);
        gl.vertex_attrib_pointer_f32(1, 2, glow::FLOAT, false, stride, 12);
        gl.enable_vertex_attrib_array(2);
        gl.vertex_attrib_pointer_f32(2, 1, glow::FLOAT, false, stride, 20);
        gl.bind_vertex_array(None);
        self.meshes.insert(
            mesh.id,
            GlMesh {
                vao,
                vbo,
                ebo,
                count: mesh.indices.len() as i32,
                used: self.frame,
            },
        );
        Some(())
    }

    unsafe fn upload_texture(&mut self, gl: &glow::Context, t: &Texture) -> Option<()> {
        if self.textures.contains_key(&t.id) {
            return Some(());
        }
        let [w, h] = t.image.size;
        let texture = gl.create_texture().ok()?;
        gl.active_texture(glow::TEXTURE0);
        gl.bind_texture(glow::TEXTURE_2D, Some(texture));
        let bytes: Vec<u8> = t.image.pixels.iter().flat_map(|c| c.to_array()).collect();
        gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
        gl.tex_image_2d(
            glow::TEXTURE_2D,
            0,
            glow::RGBA8 as i32,
            w as i32,
            h as i32,
            0,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            glow::PixelUnpackData::Slice(Some(&bytes)),
        );
        gl.generate_mipmap(glow::TEXTURE_2D);
        gl.tex_parameter_i32(
            glow::TEXTURE_2D,
            glow::TEXTURE_MIN_FILTER,
            glow::LINEAR_MIPMAP_LINEAR as i32,
        );
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, glow::LINEAR as i32);
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE as i32);
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE as i32);
        if let Some(a) = self.anisotropy {
            gl.tex_parameter_f32(glow::TEXTURE_2D, 0x84FE, a);
        }
        self.textures.insert(
            t.id,
            GlTexture {
                texture,
                used: self.frame,
            },
        );
        Some(())
    }

    unsafe fn draw(&mut self, gl: &glow::Context, scene: &Scene, item: &Item, uploads: &mut usize) {
        if self.upload_mesh(gl, &item.mesh).is_none() {
            return;
        }
        let mut texture = None;
        if let Some(t) = &item.texture {
            if self.textures.contains_key(&t.id) {
                texture = Some(t.id);
            } else if *uploads < UPLOADS_PER_FRAME && self.upload_texture(gl, t).is_some() {
                *uploads += 1;
                texture = Some(t.id);
            }
        }
        let frame = self.frame;
        let Some(mesh) = self.meshes.get_mut(&item.mesh.id) else {
            return;
        };
        mesh.used = frame;
        let u = &self.uniforms;
        let off = (
            ((item.origin.0 - scene.centre.0) * scene.k) as f32,
            ((item.origin.1 - scene.centre.1) * scene.k) as f32,
            (-scene.ground) as f32,
        );
        gl.uniform_3_f32(u.off.as_ref(), off.0, off.1, off.2);
        gl.uniform_1_f32(u.flat.as_ref(), if item.flat { 1.0 } else { 0.0 });
        gl.uniform_4_f32(u.uv.as_ref(), item.uv[0], item.uv[1], item.uv[2], item.uv[3]);
        gl.uniform_3_f32(u.color.as_ref(), item.color[0], item.color[1], item.color[2]);
        gl.uniform_1_f32(u.alpha.as_ref(), item.alpha);
        match texture.and_then(|id| self.textures.get_mut(&id)) {
            Some(t) => {
                t.used = frame;
                gl.uniform_1_f32(u.use_tex.as_ref(), 1.0);
                gl.bind_texture(glow::TEXTURE_2D, Some(t.texture));
            }
            None => gl.uniform_1_f32(u.use_tex.as_ref(), 0.0),
        }
        gl.bind_vertex_array(Some(mesh.vao));
        gl.draw_elements(glow::TRIANGLES, mesh.count, glow::UNSIGNED_INT, 0);
    }

    unsafe fn forget_unused(&mut self, gl: &glow::Context) {
        let frame = self.frame;
        self.meshes.retain(|_, m| {
            let keep = m.used + KEEP_FRAMES >= frame;
            if !keep {
                gl.delete_vertex_array(m.vao);
                gl.delete_buffer(m.vbo);
                gl.delete_buffer(m.ebo);
            }
            keep
        });
        self.textures.retain(|_, t| {
            let keep = t.used + KEEP_FRAMES >= frame;
            if !keep {
                gl.delete_texture(t.texture);
            }
            keep
        });
        for id in over_budget(self.meshes.iter().map(|(k, m)| (*k, m.used)), MAX_MESHES, frame) {
            if let Some(m) = self.meshes.remove(&id) {
                gl.delete_vertex_array(m.vao);
                gl.delete_buffer(m.vbo);
                gl.delete_buffer(m.ebo);
            }
        }
        for id in over_budget(self.textures.iter().map(|(k, t)| (*k, t.used)), MAX_TEXTURES, frame) {
            if let Some(t) = self.textures.remove(&id) {
                gl.delete_texture(t.texture);
            }
        }
    }
}

/// The ids to drop so that at most `max` remain: the least recently used
/// first, none that was used in `frame`.
fn over_budget(entries: impl Iterator<Item = (u64, u64)>, max: usize, frame: u64) -> Vec<u64> {
    let mut all: Vec<(u64, u64)> = entries.collect();
    if all.len() <= max {
        return Vec::new();
    }
    all.sort_by_key(|(_, used)| *used);
    let excess = all.len() - max;
    all.into_iter()
        .filter(|(_, used)| *used < frame)
        .take(excess)
        .map(|(id, _)| id)
        .collect()
}

fn as_bytes<T: Copy>(v: &[T]) -> &[u8] {
    // SAFETY: f32 and u32 have no padding and any bit pattern is a valid u8.
    unsafe { std::slice::from_raw_parts(v.as_ptr().cast::<u8>(), std::mem::size_of_val(v)) }
}

/// Why the 3D view cannot draw on this machine, once that is known.
pub fn failure() -> Option<String> {
    let guard = GPU.lock().unwrap_or_else(|e| e.into_inner());
    match guard.as_ref() {
        Some(Err(e)) => Some(e.clone()),
        _ => None,
    }
}

/// Draws `scene` into the callback's viewport: depth cleared inside the
/// clip, ground opaque, buildings blended after it.
///
/// # Safety
/// Must be called on egui_glow's context, from inside its paint.
pub unsafe fn paint(gl: &glow::Context, scene: &Scene) {
    let mut guard = GPU.lock().unwrap_or_else(|e| e.into_inner());
    if guard.is_none() {
        let made = create(gl);
        if let Err(e) = &made {
            eprintln!("corvus-gcs: no 3D map: {e}");
        }
        *guard = Some(made);
    }
    let Some(Ok(gpu)) = guard.as_mut() else { return };
    gpu.frame += 1;

    gl.depth_mask(true);
    gl.clear_depth_f32(1.0);
    gl.clear(glow::DEPTH_BUFFER_BIT);
    gl.enable(glow::DEPTH_TEST);
    gl.depth_func(glow::LEQUAL);
    gl.disable(glow::CULL_FACE);

    gl.use_program(Some(gpu.program));
    gl.uniform_matrix_4_f32_slice(gpu.uniforms.mvp.as_ref(), false, &scene.mvp);
    gl.uniform_1_f32(gpu.uniforms.k.as_ref(), scene.k as f32);
    gl.uniform_1_i32(gpu.uniforms.tex.as_ref(), 0);
    gl.active_texture(glow::TEXTURE0);

    let mut uploads = 0;
    gl.disable(glow::BLEND);
    for item in &scene.ground_items {
        gpu.draw(gl, scene, item, &mut uploads);
    }
    if !scene.building_items.is_empty() {
        gl.enable(glow::BLEND);
        gl.blend_func(glow::SRC_ALPHA, glow::ONE_MINUS_SRC_ALPHA);
        for item in &scene.building_items {
            gpu.draw(gl, scene, item, &mut uploads);
        }
        gl.disable(glow::BLEND);
    }
    if !scene.line_items.is_empty() {
        gl.enable(glow::BLEND);
        gl.blend_func(glow::SRC_ALPHA, glow::ONE_MINUS_SRC_ALPHA);
        gl.enable(glow::POLYGON_OFFSET_FILL);
        gl.polygon_offset(-2.0, -2.0);
        for item in &scene.line_items {
            gpu.draw(gl, scene, item, &mut uploads);
        }
        gl.disable(glow::POLYGON_OFFSET_FILL);
        gl.disable(glow::BLEND);
    }
    gl.bind_vertex_array(None);
    gl.disable(glow::DEPTH_TEST);
    gpu.forget_unused(gl);
}

static RELEASE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Asks for every GL object to go at the next paint of the flat map, which is
/// the first thing that has the context once the 3D view is left.
pub fn request_release() {
    RELEASE.store(true, Ordering::Relaxed);
}

pub fn release_pending() -> bool {
    RELEASE.load(Ordering::Relaxed)
}

/// Frees the GL objects when a release was asked for; the next 3D paint makes
/// them again.
///
/// # Safety
/// Must be called on egui_glow's context, from inside its paint.
pub unsafe fn release_if_requested(gl: &glow::Context) {
    if RELEASE.swap(false, Ordering::Relaxed) {
        destroy(gl);
    }
}

/// Frees the GL objects. Called with the context on the way out.
pub fn destroy(gl: &glow::Context) {
    let mut guard = GPU.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(Ok(gpu)) = guard.take() {
        // SAFETY: the objects were made on this context and nothing uses
        // them once the window is closing.
        unsafe {
            gl.delete_program(gpu.program);
            for m in gpu.meshes.into_values() {
                gl.delete_vertex_array(m.vao);
                gl.delete_buffer(m.vbo);
                gl.delete_buffer(m.ebo);
            }
            for t in gpu.textures.into_values() {
                gl.delete_texture(t.texture);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_least_recently_used_go_first_and_the_current_frame_stays() {
        let entries = [(1, 5), (2, 9), (3, 1), (4, 10), (5, 3)];
        assert_eq!(over_budget(entries.into_iter(), 5, 10), Vec::<u64>::new());
        assert_eq!(over_budget(entries.into_iter(), 3, 10), vec![3, 5]);
        assert_eq!(
            over_budget(entries.into_iter(), 0, 10),
            vec![3, 5, 1, 2],
            "id 4 was drawn this frame"
        );
    }

    #[test]
    fn ids_are_never_reused() {
        let a = next_id();
        let b = next_id();
        assert!(b > a);
    }

    #[test]
    fn a_mesh_has_whole_vertices() {
        let m = MeshData::new(vec![0.0; FLOATS * 3], vec![0, 1, 2]);
        assert_eq!(m.verts.len() / FLOATS, 3);
    }
}
