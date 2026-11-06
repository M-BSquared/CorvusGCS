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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use eframe::egui;
use eframe::egui_glow;
use eframe::glow::{self, HasContext};

/// Floats per vertex: position (3), texture coordinate (2), shade (1).
pub const FLOATS: usize = 6;
/// Frames a mesh or a texture may go unused before the GPU lets it go.
const KEEP_FRAMES: u64 = 240;
/// Textures uploaded in one frame at most, so a pan does not stall on them.
const UPLOADS_PER_FRAME: usize = 24;
/// Meshes uploaded in one frame at most, for the same reason.
const MESH_UPLOADS_PER_FRAME: usize = 64;
/// What the GPU holds at most. Past it the longest unused go first, so a long
/// session over the whole world stays bounded whatever the camera did.
pub const MAX_MESHES: usize = 2400;
pub const MAX_TEXTURES: usize = 700;
/// `CORVUS_GUI_3D_FAIL=reason` makes the renderer report that reason as if the
/// OpenGL had refused it, to look at the fallback without a broken driver.
const FAIL_ENV: &str = "CORVUS_GUI_3D_FAIL";

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
    /// What to draw while `texture` is not on the GPU yet: the nearest
    /// ancestor's image and its own `uv`, so a tile arriving never blinks.
    pub fallback: Option<(Texture, [f32; 4])>,
    /// Draw at the view centre's height, for ground with no elevation yet.
    pub flat: bool,
    /// What the vertex shade multiplies where there is no texture.
    pub color: [f32; 3],
    pub alpha: f32,
    /// A mesh made for this frame only (a ribbon follows the camera): it is
    /// uploaded, drawn and freed in the same paint, and never kept.
    pub transient: bool,
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

/// What a frame cost, for the debug overlay and for tests.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Stats {
    /// Leaves of ground drawn, and meshes built on the CPU for them this frame.
    pub leaves: usize,
    pub built: usize,
    pub draws: u32,
    pub meshes_uploaded: u32,
    pub textures_uploaded: u32,
    pub transient: u32,
    /// Something waited for the next frame because the budget was spent.
    pub deferred: bool,
    pub live_meshes: usize,
    pub live_textures: usize,
    /// Milliseconds the view took to plan the frame, and the GL calls to submit it.
    pub plan_ms: f32,
    pub submit_ms: f32,
}

static STATS: Mutex<Stats> = Mutex::new(Stats {
    leaves: 0,
    built: 0,
    draws: 0,
    meshes_uploaded: 0,
    textures_uploaded: 0,
    transient: 0,
    deferred: false,
    live_meshes: 0,
    live_textures: 0,
    plan_ms: 0.0,
    submit_ms: 0.0,
});
static DEFERRED: AtomicBool = AtomicBool::new(false);

/// The last finished frame.
pub fn stats() -> Stats {
    *STATS.lock().unwrap_or_else(|e| e.into_inner())
}

/// The view's side of the numbers: what it planned and how long that took.
pub fn note_plan(leaves: usize, built: usize, ms: f32) {
    let mut s = STATS.lock().unwrap_or_else(|e| e.into_inner());
    s.leaves = leaves;
    s.built = built;
    s.plan_ms = ms;
}

/// Whether the last frame left work for the next one, so the view asks for it.
pub fn deferred() -> bool {
    DEFERRED.load(Ordering::Relaxed)
}

/// Objects the GPU keeps by id, each with the frame it was last drawn in.
struct Pool<V> {
    items: HashMap<u64, (V, u64)>,
}

impl<V> Pool<V> {
    fn new() -> Pool<V> {
        Pool { items: HashMap::new() }
    }

    fn contains(&self, id: u64) -> bool {
        self.items.contains_key(&id)
    }

    fn len(&self) -> usize {
        self.items.len()
    }

    fn insert(&mut self, id: u64, v: V, frame: u64) {
        self.items.insert(id, (v, frame));
    }

    /// The object, marked as drawn in `frame`.
    fn touch(&mut self, id: u64, frame: u64) -> Option<&V> {
        self.items.get_mut(&id).map(|(v, used)| {
            *used = frame;
            &*v
        })
    }

    /// Lets go of what has gone unused for `keep` frames, then of the longest
    /// unused until no more than `max` are left. What `frame` drew stays.
    fn sweep(&mut self, frame: u64, keep: u64, max: usize, mut free: impl FnMut(V)) {
        let old: Vec<u64> = self
            .items
            .iter()
            .filter(|(_, (_, used))| used + keep < frame)
            .map(|(id, _)| *id)
            .collect();
        for id in old {
            if let Some((v, _)) = self.items.remove(&id) {
                free(v);
            }
        }
        if self.items.len() > max {
            let mut by_age: Vec<(u64, u64)> = self
                .items
                .iter()
                .filter(|(_, (_, used))| *used < frame)
                .map(|(id, (_, used))| (*used, *id))
                .collect();
            by_age.sort_unstable();
            for (_, id) in by_age.into_iter().take(self.items.len() - max) {
                if let Some((v, _)) = self.items.remove(&id) {
                    free(v);
                }
            }
        }
    }

    fn drain(&mut self, mut free: impl FnMut(V)) {
        for (_, (v, _)) in self.items.drain() {
            free(v);
        }
    }
}

struct GlMesh {
    vao: glow::VertexArray,
    vbo: glow::Buffer,
    ebo: glow::Buffer,
    count: i32,
}

struct GlTexture {
    texture: glow::Texture,
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
    meshes: Pool<GlMesh>,
    textures: Pool<GlTexture>,
    /// Meshes made for one frame, freed when it is drawn.
    scratch: Vec<GlMesh>,
    frame: u64,
    anisotropy: Option<f32>,
}

/// What a frame may still upload, and what it did.
#[derive(Default)]
struct Budget {
    textures: usize,
    meshes: usize,
    transient: u32,
    draws: u32,
    deferred: bool,
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

/// What the OpenGL context offers, as far as the 3D view needs to know.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Caps {
    /// GLSL with `in` and `out` (330 core, or ES 300).
    pub new_glsl: bool,
    /// Bits of the window's depth buffer; `None` when the driver would not say.
    pub depth_bits: Option<i32>,
    /// Largest texture side; 0 when unknown.
    pub max_texture: i32,
}

/// Whether the 3D view can run on a context with these `caps`, and why not.
pub fn check_caps(c: &Caps) -> Result<(), String> {
    if !c.new_glsl {
        return Err("this OpenGL has no GLSL with `in` and `out`".into());
    }
    if c.depth_bits == Some(0) {
        return Err("the window has no depth buffer".into());
    }
    if c.max_texture > 0 && c.max_texture < 256 {
        return Err(format!(
            "this OpenGL cannot hold a 256 pixel map tile (its limit is {})",
            c.max_texture
        ));
    }
    Ok(())
}

unsafe fn read_caps(gl: &glow::Context) -> Caps {
    let version = egui_glow::ShaderVersion::get(gl);
    while gl.get_error() != glow::NO_ERROR {}
    let mut depth = gl.get_framebuffer_attachment_parameter_i32(
        glow::FRAMEBUFFER,
        glow::DEPTH,
        glow::FRAMEBUFFER_ATTACHMENT_DEPTH_SIZE,
    );
    if gl.get_error() != glow::NO_ERROR {
        depth = -1;
    }
    if depth == 0 {
        // A compatibility context answers this one where the first is refused.
        let old = gl.get_parameter_i32(glow::DEPTH_BITS);
        depth = if gl.get_error() == glow::NO_ERROR { old } else { 0 };
    }
    while gl.get_error() != glow::NO_ERROR {}
    let max_texture = gl.get_parameter_i32(glow::MAX_TEXTURE_SIZE);
    while gl.get_error() != glow::NO_ERROR {}
    Caps {
        new_glsl: version.is_new_shader_interface(),
        depth_bits: (depth >= 0).then_some(depth),
        max_texture,
    }
}

unsafe fn compile(gl: &glow::Context, kind: u32, header: &str, body: &str) -> Result<glow::Shader, String> {
    let shader = gl.create_shader(kind)?;
    gl.shader_source(shader, &format!("{header}\nprecision highp float;\n{body}"));
    gl.compile_shader(shader);
    if gl.get_shader_compile_status(shader) {
        Ok(shader)
    } else {
        let log = gl.get_shader_info_log(shader);
        gl.delete_shader(shader);
        Err(format!("a shader does not compile: {}", log.trim()))
    }
}

unsafe fn create(gl: &glow::Context) -> Result<Gpu, String> {
    if let Ok(reason) = std::env::var(FAIL_ENV) {
        return Err(reason);
    }
    check_caps(&read_caps(gl))?;
    let version = egui_glow::ShaderVersion::get(gl);
    let header = version.version_declaration();
    let vs = compile(gl, glow::VERTEX_SHADER, header, VERTEX)?;
    let fs = match compile(gl, glow::FRAGMENT_SHADER, header, FRAGMENT) {
        Ok(fs) => fs,
        Err(e) => {
            gl.delete_shader(vs);
            return Err(e);
        }
    };
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
        return Err(format!("the shaders do not link: {}", log.trim()));
    }
    let u = |name: &str| gl.get_uniform_location(program, name);
    let anisotropy = gl
        .supported_extensions()
        .iter()
        .any(|e| e.ends_with("texture_filter_anisotropic"))
        .then(|| gl.get_parameter_f32(0x84FF).clamp(1.0, 8.0));
    while gl.get_error() != glow::NO_ERROR {}
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
        meshes: Pool::new(),
        textures: Pool::new(),
        scratch: Vec::new(),
        frame: 0,
        anisotropy,
    })
}

unsafe fn make_mesh(gl: &glow::Context, mesh: &MeshData) -> Option<GlMesh> {
    if mesh.indices.is_empty() {
        return None;
    }
    let vao = gl.create_vertex_array().ok()?;
    let Ok(vbo) = gl.create_buffer() else {
        gl.delete_vertex_array(vao);
        return None;
    };
    let Ok(ebo) = gl.create_buffer() else {
        gl.delete_vertex_array(vao);
        gl.delete_buffer(vbo);
        return None;
    };
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
    Some(GlMesh {
        vao,
        vbo,
        ebo,
        count: mesh.indices.len() as i32,
    })
}

unsafe fn free_mesh(gl: &glow::Context, m: &GlMesh) {
    gl.delete_vertex_array(m.vao);
    gl.delete_buffer(m.vbo);
    gl.delete_buffer(m.ebo);
}

impl Gpu {
    unsafe fn upload_texture(&mut self, gl: &glow::Context, t: &Texture) -> Option<()> {
        if self.textures.contains(t.id) {
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
        self.textures.insert(t.id, GlTexture { texture }, self.frame);
        Some(())
    }

    /// The first of the item's images that is on the GPU or fits this
    /// frame's upload budget, with the part of it the item covers.
    unsafe fn pick_texture(&mut self, gl: &glow::Context, item: &Item, b: &mut Budget) -> Option<(u64, [f32; 4])> {
        let wanted = item.texture.iter().map(|t| (t, item.uv));
        let spare = item.fallback.iter().map(|(t, uv)| (t, *uv));
        for (t, uv) in wanted.chain(spare) {
            if self.textures.contains(t.id) {
                return Some((t.id, uv));
            }
            if b.textures < UPLOADS_PER_FRAME {
                if self.upload_texture(gl, t).is_some() {
                    b.textures += 1;
                    return Some((t.id, uv));
                }
            } else {
                b.deferred = true;
            }
        }
        None
    }

    unsafe fn draw(&mut self, gl: &glow::Context, scene: &Scene, item: &Item, b: &mut Budget) {
        let (vao, count) = if item.transient {
            let Some(m) = make_mesh(gl, &item.mesh) else {
                return;
            };
            let r = (m.vao, m.count);
            self.scratch.push(m);
            b.transient += 1;
            r
        } else {
            if !self.meshes.contains(item.mesh.id) {
                if b.meshes >= MESH_UPLOADS_PER_FRAME {
                    b.deferred = true;
                    return;
                }
                let Some(m) = make_mesh(gl, &item.mesh) else {
                    return;
                };
                self.meshes.insert(item.mesh.id, m, self.frame);
                b.meshes += 1;
            }
            match self.meshes.touch(item.mesh.id, self.frame) {
                Some(m) => (m.vao, m.count),
                None => return,
            }
        };
        let picked = self.pick_texture(gl, item, b);
        let u = &self.uniforms;
        let off = (
            ((item.origin.0 - scene.centre.0) * scene.k) as f32,
            ((item.origin.1 - scene.centre.1) * scene.k) as f32,
            (-scene.ground) as f32,
        );
        let uv = picked.map_or(item.uv, |(_, uv)| uv);
        gl.uniform_3_f32(u.off.as_ref(), off.0, off.1, off.2);
        gl.uniform_1_f32(u.flat.as_ref(), if item.flat { 1.0 } else { 0.0 });
        gl.uniform_4_f32(u.uv.as_ref(), uv[0], uv[1], uv[2], uv[3]);
        gl.uniform_3_f32(u.color.as_ref(), item.color[0], item.color[1], item.color[2]);
        gl.uniform_1_f32(u.alpha.as_ref(), item.alpha);
        match picked.and_then(|(id, _)| self.textures.touch(id, self.frame)) {
            Some(t) => {
                gl.uniform_1_f32(u.use_tex.as_ref(), 1.0);
                gl.bind_texture(glow::TEXTURE_2D, Some(t.texture));
            }
            None => gl.uniform_1_f32(u.use_tex.as_ref(), 0.0),
        }
        gl.bind_vertex_array(Some(vao));
        gl.draw_elements(glow::TRIANGLES, count, glow::UNSIGNED_INT, 0);
        b.draws += 1;
    }

    unsafe fn forget_unused(&mut self, gl: &glow::Context) {
        for m in self.scratch.drain(..) {
            free_mesh(gl, &m);
        }
        let frame = self.frame;
        self.meshes.sweep(frame, KEEP_FRAMES, MAX_MESHES, |m| free_mesh(gl, &m));
        self.textures
            .sweep(frame, KEEP_FRAMES, MAX_TEXTURES, |t| gl.delete_texture(t.texture));
    }
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
    let started = std::time::Instant::now();
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

    let mut budget = Budget::default();
    gl.disable(glow::BLEND);
    for item in &scene.ground_items {
        gpu.draw(gl, scene, item, &mut budget);
    }
    if !scene.building_items.is_empty() {
        gl.enable(glow::BLEND);
        gl.blend_func(glow::SRC_ALPHA, glow::ONE_MINUS_SRC_ALPHA);
        for item in &scene.building_items {
            gpu.draw(gl, scene, item, &mut budget);
        }
        gl.disable(glow::BLEND);
    }
    if !scene.line_items.is_empty() {
        gl.enable(glow::BLEND);
        gl.blend_func(glow::SRC_ALPHA, glow::ONE_MINUS_SRC_ALPHA);
        gl.enable(glow::POLYGON_OFFSET_FILL);
        gl.polygon_offset(-2.0, -2.0);
        for item in &scene.line_items {
            gpu.draw(gl, scene, item, &mut budget);
        }
        gl.disable(glow::POLYGON_OFFSET_FILL);
        gl.disable(glow::BLEND);
    }
    gl.bind_vertex_array(None);
    gl.disable(glow::DEPTH_TEST);
    gpu.forget_unused(gl);
    DEFERRED.store(budget.deferred, Ordering::Relaxed);
    let mut s = STATS.lock().unwrap_or_else(|e| e.into_inner());
    s.draws = budget.draws;
    s.meshes_uploaded = budget.meshes as u32;
    s.textures_uploaded = budget.textures as u32;
    s.transient = budget.transient;
    s.deferred = budget.deferred;
    s.live_meshes = gpu.meshes.len();
    s.live_textures = gpu.textures.len();
    s.submit_ms = started.elapsed().as_secs_f32() * 1000.0;
}

/// Frees the GL objects. Called with the context on the way out.
pub fn destroy(gl: &glow::Context) {
    let mut guard = GPU.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(Ok(mut gpu)) = guard.take() {
        // SAFETY: the objects were made on this context and nothing uses
        // them once the window is closing.
        unsafe {
            gl.delete_program(gpu.program);
            gpu.meshes.drain(|m| free_mesh(gl, &m));
            gpu.textures.drain(|t| gl.delete_texture(t.texture));
            for m in gpu.scratch.drain(..) {
                free_mesh(gl, &m);
            }
        }
    }
    *STATS.lock().unwrap_or_else(|e| e.into_inner()) = Stats::default();
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn caps() -> Caps {
        Caps {
            new_glsl: true,
            depth_bits: Some(24),
            max_texture: 8192,
        }
    }

    #[test]
    fn a_good_context_is_accepted() {
        assert_eq!(check_caps(&caps()), Ok(()));
        let unknown = Caps {
            depth_bits: None,
            max_texture: 0,
            ..caps()
        };
        assert_eq!(check_caps(&unknown), Ok(()), "a driver that will not say is trusted");
    }

    #[test]
    fn a_context_that_cannot_run_the_view_says_why() {
        let old = Caps {
            new_glsl: false,
            ..caps()
        };
        assert!(check_caps(&old).unwrap_err().contains("GLSL"));
        let flat = Caps {
            depth_bits: Some(0),
            ..caps()
        };
        assert!(check_caps(&flat).unwrap_err().contains("depth buffer"));
        let tiny = Caps {
            max_texture: 128,
            ..caps()
        };
        assert!(check_caps(&tiny).unwrap_err().contains("256 pixel"));
    }

    #[test]
    fn a_pool_lets_go_of_what_went_unused() {
        let mut pool: Pool<u32> = Pool::new();
        for i in 0..1000u64 {
            pool.insert(i, i as u32, i);
            pool.sweep(i, 10, usize::MAX, |_| {});
            assert!(pool.len() <= 11, "frame {i}: {} held", pool.len());
        }
        let mut freed = Vec::new();
        pool.sweep(5000, 10, usize::MAX, |v| freed.push(v));
        assert_eq!(pool.len(), 0, "an idle view holds nothing");
        assert_eq!(freed.len(), 11);
    }

    #[test]
    fn a_pool_stays_under_its_bound_however_fast_it_fills() {
        let mut pool: Pool<u32> = Pool::new();
        let mut freed = 0;
        for i in 0..5000u64 {
            for j in 0..10 {
                pool.insert(i * 10 + j, 0, i);
            }
            pool.sweep(i + 1, KEEP_FRAMES, 300, |_| freed += 1);
            assert!(pool.len() <= 300, "frame {i}: {} held", pool.len());
        }
        assert_eq!(freed + pool.len(), 50_000, "every object was freed or is still held");
    }

    #[test]
    fn a_pool_keeps_what_the_frame_drew_even_past_its_bound() {
        let mut pool: Pool<u32> = Pool::new();
        for i in 0..20u64 {
            pool.insert(i, 0, 7);
        }
        pool.insert(100, 0, 1);
        let mut freed = 0;
        pool.sweep(7, 240, 5, |_| freed += 1);
        assert_eq!(freed, 1, "only the one not drawn this frame goes");
        assert_eq!(pool.len(), 20);
        let mut left = 0;
        pool.drain(|_| left += 1);
        assert_eq!(left, 20);
        assert_eq!(pool.len(), 0);
    }

    #[test]
    fn touching_marks_an_object_used() {
        let mut pool: Pool<u32> = Pool::new();
        pool.insert(1, 10, 0);
        pool.insert(2, 20, 0);
        assert_eq!(pool.touch(1, 500).copied(), Some(10));
        let mut freed = Vec::new();
        pool.sweep(500, 240, 100, |v| freed.push(v));
        assert_eq!(freed, vec![20]);
        assert!(pool.touch(3, 500).is_none());
    }

    #[test]
    fn a_refused_context_is_reported_and_clears_with_the_gpu() {
        *GPU.lock().unwrap() = Some(Err("no depth buffer".into()));
        assert_eq!(failure().as_deref(), Some("no depth buffer"));
        *GPU.lock().unwrap() = None;
        assert_eq!(failure(), None);
    }
}
