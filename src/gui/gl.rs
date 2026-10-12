//! The base map and walk graph drawn with OpenGL: one vertex buffer of
//! triangles and line segments in layers, uploaded once per graph; each
//! frame only sets the view.

use crate::basemap::Basemap;
use crate::graph::{Graph, Xy};
use crate::osm::Kind;
use egui_glow::glow::{self, HasContext};

/// Layers, drawn in this order.
pub const WATER_FILL: usize = 0;
pub const PARK_FILL: usize = 1;
pub const WATER_LINE: usize = 2;
pub const BUSY_ROAD: usize = 3;
pub const ROAD: usize = 4;
pub const BESIDE: usize = 5;
pub const PATH: usize = 6;
pub const LAYERS: usize = 7;

/// Layers drawn as triangles (the rest are line segments).
const FILLS: [usize; 2] = [WATER_FILL, PARK_FILL];
/// Lines drawn this many times, a pixel apart, to look thicker.
const THICK: [(usize, &[[f32; 2]]); 2] =
    [(WATER_LINE, &[[0.0, 0.0], [1.0, 0.0]]), (BUSY_ROAD, &[[0.0, 0.0], [1.0, 0.0], [0.0, 1.0], [1.0, 1.0]])];

/// Vertex data for every layer, as flat x,y pairs.
pub struct Vertices {
    data: Vec<f32>,
    counts: [i32; LAYERS],
}

pub fn vertices(g: &Graph, base: &Basemap) -> Vertices {
    let mut layers: [Vec<f32>; LAYERS] = Default::default();
    let flat = |pts: &[Xy]| pts.iter().flat_map(|p| [p[0] as f32, p[1] as f32]).collect::<Vec<f32>>();
    layers[WATER_FILL] = flat(&base.water);
    layers[PARK_FILL] = flat(&base.parks);
    layers[WATER_LINE] = flat(&base.water_lines);
    layers[BUSY_ROAD] = flat(&base.roads);
    for e in &g.edges {
        let layer = match (e.kind, e.beside) {
            (Kind::Road, _) => ROAD,
            (Kind::CarFree, true) => BESIDE,
            (Kind::CarFree, false) => PATH,
        };
        for w in e.pts.windows(2) {
            layers[layer].extend_from_slice(&[w[0][0] as f32, w[0][1] as f32, w[1][0] as f32, w[1][1] as f32]);
        }
    }
    let counts = std::array::from_fn(|i| (layers[i].len() / 2) as i32);
    Vertices { data: layers.concat(), counts }
}

/// The graph's vertex buffer and shader, created lazily on first paint.
#[derive(Default)]
pub struct GlGraph {
    objects: Option<(glow::Program, glow::VertexArray, glow::Buffer)>,
    /// New vertex data to upload on the next paint.
    pub pending: Option<Vertices>,
    counts: [i32; LAYERS],
}

const VERTEX: &str = r#"
    IN vec2 a_pos;
    uniform vec2 u_center;
    uniform vec2 u_scale;
    uniform vec2 u_offset;
    void main() {
        gl_Position = vec4((a_pos - u_center) * u_scale + u_offset, 0.0, 1.0);
    }
"#;
const FRAGMENT: &str = r#"
    precision mediump float;
    uniform vec4 u_color;
    OUT_DECL
    void main() {
        OUT = u_color;
    }
"#;

impl GlGraph {
    /// Draws the layers with a colour (None: hidden); `pixel` is one
    /// screen pixel in clip space.
    pub fn paint(
        &mut self,
        gl: &glow::Context,
        center: Xy,
        scale: [f32; 2],
        pixel: [f32; 2],
        colors: [Option<[f32; 4]>; LAYERS],
    ) {
        unsafe {
            if self.objects.is_none() {
                match setup(gl) {
                    Ok(o) => self.objects = Some(o),
                    Err(e) => {
                        eprintln!("map: OpenGL setup failed: {e}");
                        return;
                    }
                }
            }
            let (program, vao, vbo) = self.objects.unwrap();
            if let Some(v) = self.pending.take() {
                gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
                let bytes = std::slice::from_raw_parts(v.data.as_ptr() as *const u8, v.data.len() * 4);
                gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, bytes, glow::STATIC_DRAW);
                self.counts = v.counts;
            }
            gl.use_program(Some(program));
            gl.bind_vertex_array(Some(vao));
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
            gl.enable_vertex_attrib_array(0);
            gl.vertex_attrib_pointer_f32(0, 2, glow::FLOAT, false, 8, 0);
            // Positions are relative to the view centre in f32; subtracting
            // a large centre in the shader would lose precision far out,
            // but the extract is small (tens of km), so metres suffice.
            gl.uniform_2_f32(gl.get_uniform_location(program, "u_center").as_ref(), center[0] as f32, center[1] as f32);
            gl.uniform_2_f32(gl.get_uniform_location(program, "u_scale").as_ref(), scale[0], scale[1]);
            let color = gl.get_uniform_location(program, "u_color");
            let offset = gl.get_uniform_location(program, "u_offset");
            let mut first = 0;
            for (layer, (c, &n)) in colors.iter().zip(&self.counts).enumerate() {
                if let Some(c) = c {
                    gl.uniform_4_f32_slice(color.as_ref(), c);
                    let mode = if FILLS.contains(&layer) { glow::TRIANGLES } else { glow::LINES };
                    let shifts = THICK.iter().find(|t| t.0 == layer).map_or(&[[0.0, 0.0]][..], |t| t.1);
                    for d in shifts {
                        gl.uniform_2_f32(offset.as_ref(), d[0] * pixel[0], d[1] * pixel[1]);
                        gl.draw_arrays(mode, first, n);
                    }
                }
                first += n;
            }
            gl.uniform_2_f32(offset.as_ref(), 0.0, 0.0);
            gl.bind_vertex_array(None);
        }
    }
}

unsafe fn setup(gl: &glow::Context) -> Result<(glow::Program, glow::VertexArray, glow::Buffer), String> {
    let version = egui_glow::ShaderVersion::get(gl);
    let new = version.is_new_shader_interface();
    let decl = version.version_declaration();
    let vs = VERTEX.replace("IN", if new { "in" } else { "attribute" });
    let fs = if new {
        FRAGMENT.replace("OUT_DECL", "out vec4 out_color;").replace("OUT =", "out_color =")
    } else {
        FRAGMENT.replace("OUT_DECL", "").replace("OUT =", "gl_FragColor =")
    };
    unsafe {
        let program = gl.create_program()?;
        let mut shaders = vec![];
        for (kind, src) in [(glow::VERTEX_SHADER, vs), (glow::FRAGMENT_SHADER, fs)] {
            let s = gl.create_shader(kind)?;
            gl.shader_source(s, &format!("{decl}\n{src}"));
            gl.compile_shader(s);
            if !gl.get_shader_compile_status(s) {
                return Err(gl.get_shader_info_log(s));
            }
            gl.attach_shader(program, s);
            shaders.push(s);
        }
        gl.bind_attrib_location(program, 0, "a_pos");
        gl.link_program(program);
        if !gl.get_program_link_status(program) {
            return Err(gl.get_program_info_log(program));
        }
        for s in shaders {
            gl.detach_shader(program, s);
            gl.delete_shader(s);
        }
        let vao = gl.create_vertex_array()?;
        let vbo = gl.create_buffer()?;
        Ok((program, vao, vbo))
    }
}
